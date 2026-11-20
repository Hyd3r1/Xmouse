//! Frame generation for PC Play: a D3D12 picture surface over the player window.
//!
//! The player window stays what it is (egui on OpenGL, with all input, mouse capture and
//! hotkeys). While frame generation runs, a disabled child window covers the picture area
//! and shows the game through its own D3D12 swapchain, which is the only kind of swapchain
//! AMD's and NVIDIA's frame generators attach to. A disabled child receives no input: every
//! click and key still lands in the player window underneath.
//!
//! The swapchain keeps the stream's own size for the whole session and the system stretches
//! it to the window. Nothing is recreated when the window is resized or goes fullscreen,
//! which is where graphics drivers have crashed on us before.
//!
//! The vendors' DLLs are never part of XMouse: they are loaded from the folder
//! `install.rs` downloads them into, after their signatures have been checked.

mod ffi;
pub mod install;

use std::ffi::{c_char, c_void, CStr};
use std::mem::{size_of, transmute_copy, ManuallyDrop};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{Interface, BOOL, GUID, HRESULT, PCSTR, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use ffi::{ffx, sl, HLSL};

/// Which frame generator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Fsr,
    Dlss,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Fsr => "FSR",
            Kind::Dlss => "DLSS",
        }
    }
    pub fn folder(self) -> &'static str {
        match self {
            Kind::Fsr => "fsr",
            Kind::Dlss => "dlss",
        }
    }
    /// The files that must be present, and who must have signed them.
    pub fn files(self) -> (&'static [&'static str], &'static str) {
        match self {
            Kind::Fsr => (
                &["amd_fidelityfx_loader_dx12.dll", "amd_fidelityfx_framegeneration_dx12.dll"],
                "Advanced Micro Devices",
            ),
            Kind::Dlss => (
                &["sl.interposer.dll", "sl.common.dll", "sl.dlss_g.dll", "nvngx_dlssg.dll", "sl.reflex.dll", "sl.pcl.dll"],
                "NVIDIA",
            ),
        }
    }
}

/// Where a generator's files are, if all of them are there. `XMOUSE_FSR_DIR` /
/// `XMOUSE_DLSS_DIR` point somewhere else for testing.
pub fn dll_dir(kind: Kind, config_dir: &Path) -> Option<PathBuf> {
    let var = match kind {
        Kind::Fsr => "XMOUSE_FSR_DIR",
        Kind::Dlss => "XMOUSE_DLSS_DIR",
    };
    let dir = std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir.join("framegen").join(kind.folder()));
    kind.files().0.iter().all(|f| dir.join(f).is_file()).then_some(dir)
}

/// Is there an NVIDIA graphics card in this PC? (Whether it can do DLSS frame generation is
/// only known once NVIDIA's own library has been asked, at the start of a session.)
pub fn nvidia_present() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return false };
        let mut i = 0;
        while let Ok(a) = factory.EnumAdapters1(i) {
            if a.GetDesc1().map(|d| d.VendorId == 0x10DE).unwrap_or(false) {
                return true;
            }
            i += 1;
        }
        false
    })
}

/// This thread's top-level window whose title starts with `prefix` (the player window).
pub fn find_own_window(prefix: &str) -> Option<isize> {
    struct Search(Vec<u16>, Option<isize>);
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp.0 as *mut Search);
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf) as usize;
        if n >= s.0.len() && buf[..s.0.len()] == s.0[..] && IsWindowVisible(hwnd).as_bool() {
            s.1 = Some(hwnd.0 as isize);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut s = Search(prefix.encode_utf16().collect(), None);
    unsafe {
        let _ = EnumThreadWindows(GetCurrentThreadId(), Some(each), LPARAM(&mut s as *mut Search as isize));
    }
    s.1
}

/// One decoded picture for the surface.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub nv12: Vec<u8>,
    /// When it came out of the decoder.
    pub at: Instant,
    /// The picture has nothing to do with the one before (scene cut, first frame).
    pub cut: bool,
    /// Seconds between the game's own frames, as measured so far.
    pub interval: f64,
}

enum Cmd {
    Frame(Frame),
    Stop,
}

/// What the presenter thread tells the app.
#[derive(Default)]
pub struct Shared {
    error: Mutex<Option<String>>,
    ready: AtomicBool,
    /// Frames we presented, and frames that reached the swapchain in total.
    pub real: AtomicU64,
    pub shown: AtomicU64,
    /// Longest gap between two of our presents since the last look, in 0.1 ms.
    pub worst_gap: AtomicU32,
}

/// The picture surface and its presenter thread.
pub struct Surface {
    hwnd: isize,
    tx: mpsc::Sender<Cmd>,
    pub shared: Arc<Shared>,
    pub kind: Kind,
    pub size: [u32; 2],
    placed: [i32; 5],
}

unsafe extern "system" fn surface_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

impl Surface {
    /// Creates the (still hidden) surface window as a child of `parent` and starts the
    /// presenter for a `width` x `height` stream. Must be called on the thread that owns
    /// `parent`. Problems show up later in `error()`.
    pub fn start(kind: Kind, dir: PathBuf, parent: isize, width: u32, height: u32) -> Result<Surface> {
        let hwnd = unsafe {
            let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
            let class = wide("XMousePictureSurface");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(surface_proc),
                hInstance: hinst,
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wc); // fails harmlessly when already registered
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_DISABLED,
                0,
                0,
                16,
                16,
                Some(HWND(parent as *mut c_void)),
                None,
                Some(hinst),
                None,
            )?
        };
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let (sh, raw) = (shared.clone(), hwnd.0 as isize);
        std::thread::Builder::new()
            .name("picture-surface".into())
            .spawn(move || {
                // One surface at a time in the whole process: a session that starts while
                // the last one is still letting go of the graphics card waits here.
                static BUSY: Mutex<()> = Mutex::new(());
                let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
                let res = unsafe { present_loop(kind, &dir, HWND(raw as *mut c_void), width, height, rx, &sh) };
                if let Err(e) = res {
                    log::warn!("{} frame generation stopped: {e:#}", kind.name());
                    *sh.error.lock().unwrap() = Some(format!("{e:#}"));
                }
                // The window belongs to the UI thread; ask it to close it.
                unsafe {
                    let _ = PostMessageW(Some(HWND(raw as *mut c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            })?;
        Ok(Surface { hwnd: hwnd.0 as isize, tx, shared, kind, size: [width, height], placed: [0; 5] })
    }

    /// Why it stopped, if it did.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// The first picture is on its way to the screen.
    pub fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::Relaxed)
    }

    /// Puts the surface over this rectangle of the player window (client pixels), or hides it.
    pub fn place(&mut self, rect: [i32; 4], visible: bool) {
        let want = [rect[0], rect[1], rect[2], rect[3], visible as i32];
        if want == self.placed {
            return;
        }
        self.placed = want;
        unsafe {
            let flags = SWP_NOACTIVATE | if visible { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
            let _ = SetWindowPos(HWND(self.hwnd as *mut c_void), Some(HWND_TOP), rect[0], rect[1], rect[2].max(1), rect[3].max(1), flags);
        }
    }

    pub fn push(&self, frame: Frame) {
        let _ = self.tx.send(Cmd::Frame(frame));
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // Never wait for the presenter here: it may need this thread's message loop to let
        // go of the swapchain. It closes the window itself when it is done.
        self.place([0, 0, 1, 1], false);
        let _ = self.tx.send(Cmd::Stop);
    }
}

// ----------------------------------------------------------------------------------------
// D3D12 plumbing
// ----------------------------------------------------------------------------------------

const EXTRA_HLSL: &str = r#"
// The decoder's NV12 picture (BT.709, limited range) to RGB.
float4 psNv12(VO i) : SV_Target {
    float y = (t0.SampleLevel(sLin, i.uv, 0).r - 16.0 / 255.0) * (255.0 / 219.0);
    float2 c = (t1.SampleLevel(sLin, i.uv, 0).rg - 128.0 / 255.0) * (255.0 / 224.0);
    float3 rgb = float3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    return float4(saturate(rgb), 1);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CsConsts {
    disp: [i32; 2],
    grid: [i32; 2],
    q: [i32; 2],
    coarse_range: i32,
    pad0: i32,
    truth: [f32; 16],
}

struct Tex {
    res: ID3D12Resource,
    state: D3D12_RESOURCE_STATES,
}

/// A COM pointer lent to a D3D12 struct field without touching its reference count.
unsafe fn lend<T: Interface>(x: &T) -> ManuallyDrop<Option<T>> {
    transmute_copy(x)
}

unsafe fn barrier(res: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: lend(res),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

unsafe fn to_state(list: &ID3D12GraphicsCommandList, t: &mut Tex, after: D3D12_RESOURCE_STATES) {
    if t.state != after {
        list.ResourceBarrier(&[barrier(&t.res, t.state, after)]);
        t.state = after;
    }
}

unsafe fn make_tex(dev: &ID3D12Device, w: u32, h: u32, format: DXGI_FORMAT, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> Result<Tex> {
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: w as u64,
        Height: h,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Flags: flags,
        ..Default::default()
    };
    let mut res: Option<ID3D12Resource> = None;
    dev.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut res)?;
    Ok(Tex { res: res.ok_or_else(|| anyhow!("no texture"))?, state })
}

unsafe fn compile(source: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let (entry_c, target_c) = (format!("{entry}\0"), format!("{target}\0"));
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let r = D3DCompile(
        source.as_ptr() as *const c_void,
        source.len(),
        PCSTR::null(),
        None,
        None,
        PCSTR(entry_c.as_ptr()),
        PCSTR(target_c.as_ptr()),
        1 << 15,
        0,
        &mut code,
        Some(&mut errors),
    );
    if let Err(e) = r {
        let msg = errors
            .map(|b| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).to_string())
            .unwrap_or_default();
        bail!("shader {entry}: {e} {msg}");
    }
    code.ok_or_else(|| anyhow!("no bytecode for {entry}"))
}

fn bytecode(b: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
    unsafe { D3D12_SHADER_BYTECODE { pShaderBytecode: b.GetBufferPointer(), BytecodeLength: b.GetBufferSize() } }
}

unsafe fn proc<T: Copy>(module: HMODULE, name: &str) -> Result<T> {
    let n = format!("{name}\0");
    let p = GetProcAddress(module, PCSTR(n.as_ptr())).ok_or_else(|| anyhow!("{name} is missing from the library"))?;
    Ok(transmute_copy(&p))
}

unsafe fn load_library(path: &Path) -> Result<HMODULE> {
    let w = wide(&path.to_string_lossy());
    LoadLibraryExW(PCWSTR(w.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH).with_context(|| format!("loading {}", path.display()))
}

// Descriptor slots in the one shader-visible heap.
const SRV_Y: u32 = 0;
const SRV_UV: u32 = 1;
const SRV_FRAME: u32 = 2; // +0, +1
const SRV_LUMA: u32 = 4; // +0, +1
const SRV_MVC: u32 = 6;
const SRV_MVB: u32 = 7;
const SRV_MVM: u32 = 8;
const UAV_LUMA: u32 = 9; // +0, +1
const UAV_MVC: u32 = 11;
const UAV_MVB: u32 = 12;
const UAV_MVM: u32 = 13;
const UAV_MV: u32 = 14;

struct Gfx {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    alloc: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_value: u64,
    fence_event: HANDLE,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_inc: usize,
    heap: ID3D12DescriptorHeap,
    heap_inc: u32,
    root: ID3D12RootSignature,
    pso_nv12: ID3D12PipelineState,
    cs: [ID3D12PipelineState; 6], // luma, coarse, refine, median, upsample, truth
    y: Tex,
    uv: Tex,
    upload: ID3D12Resource,
    upload_ptr: *mut u8,
    footprints: [D3D12_PLACED_SUBRESOURCE_FOOTPRINT; 2],
    frame: [Tex; 2],
    luma: [Tex; 2],
    mvc: Tex,
    mvb: Tex,
    mvm: Tex,
    mv: Tex,
    depth: Tex,
    w: u32,
    h: u32,
}

impl Gfx {
    unsafe fn new(device: &ID3D12Device, queue: &ID3D12CommandQueue, w: u32, h: u32) -> Result<Gfx> {
        let alloc: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc, None::<&ID3D12PipelineState>)?;
        list.Close()?;
        let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
        let fence_event = CreateEventW(None, false, false, PCWSTR::null())?;
        let rtv_heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            NumDescriptors: 3,
            ..Default::default()
        })?;
        let rtv_inc = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
        let heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            NumDescriptors: 16,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            ..Default::default()
        })?;
        let heap_inc = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);

        // One root signature for everything: constants, four one-texture tables, one output
        // table, a linear sampler.
        let ranges: Vec<D3D12_DESCRIPTOR_RANGE> = (0..5)
            .map(|i| D3D12_DESCRIPTOR_RANGE {
                RangeType: if i < 4 { D3D12_DESCRIPTOR_RANGE_TYPE_SRV } else { D3D12_DESCRIPTOR_RANGE_TYPE_UAV },
                NumDescriptors: 1,
                BaseShaderRegister: if i < 4 { i } else { 0 },
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 0,
            })
            .collect();
        let mut params = vec![D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: (size_of::<CsConsts>() / 4) as u32 },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        }];
        for r in &ranges {
            params.push(D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 1, pDescriptorRanges: r },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            });
        }
        let sampler = D3D12_STATIC_SAMPLER_DESC {
            Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            MaxLOD: f32::MAX,
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            ..Default::default()
        };
        let root_desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: params.len() as u32,
            pParameters: params.as_ptr(),
            NumStaticSamplers: 1,
            pStaticSamplers: &sampler,
            Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
        };
        let mut blob: Option<ID3DBlob> = None;
        D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None).context("root signature")?;
        let blob = blob.ok_or_else(|| anyhow!("no root signature"))?;
        let root: ID3D12RootSignature =
            device.CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()))?;

        let source = format!("{HLSL}{EXTRA_HLSL}");
        let (vs, ps) = (compile(&source, "vsMain", "vs_5_0")?, compile(&source, "psNv12", "ps_5_0")?);
        let mut pso = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: lend(&root),
            VS: bytecode(&vs),
            PS: bytecode(&ps),
            SampleMask: u32::MAX,
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        pso.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
        pso.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
        pso.BlendState.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
        pso.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        let pso_nv12: ID3D12PipelineState = device.CreateGraphicsPipelineState(&pso).context("picture pipeline")?;
        let compute = |entry: &str| -> Result<ID3D12PipelineState> {
            let cs = compile(&source, entry, "cs_5_0")?;
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC { pRootSignature: lend(&root), CS: bytecode(&cs), ..Default::default() };
            device.CreateComputePipelineState(&desc).with_context(|| format!("{entry} pipeline"))
        };
        let cs = [compute("csLuma")?, compute("csCoarse")?, compute("csRefine")?, compute("csMedian")?, compute("csUpsample")?, compute("csTruth")?];

        let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        let (uav, rt) = (D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET);
        let rgba = DXGI_FORMAT_R8G8B8A8_UNORM;
        let (gw, gh, qw, qh) = (w.div_ceil(8), h.div_ceil(8), w.div_ceil(4), h.div_ceil(4));
        let y = make_tex(device, w, h, DXGI_FORMAT_R8_UNORM, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
        let uv = make_tex(device, w / 2, h / 2, DXGI_FORMAT_R8G8_UNORM, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
        let frame = [make_tex(device, w, h, rgba, rt, read)?, make_tex(device, w, h, rgba, rt, read)?];
        let luma = [make_tex(device, qw, qh, DXGI_FORMAT_R16_FLOAT, uav, read)?, make_tex(device, qw, qh, DXGI_FORMAT_R16_FLOAT, uav, read)?];
        let mvf = DXGI_FORMAT_R16G16_FLOAT;
        let (mvc, mvb, mvm) = (make_tex(device, gw, gh, mvf, uav, read)?, make_tex(device, gw, gh, mvf, uav, read)?, make_tex(device, gw, gh, mvf, uav, read)?);
        let mv = make_tex(device, w, h, mvf, uav, read)?;
        let depth = make_tex(device, w, h, DXGI_FORMAT_R32_FLOAT, rt, D3D12_RESOURCE_STATE_RENDER_TARGET)?;

        // One upload buffer for both planes.
        let mut footprints = [D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); 2];
        let (mut y_bytes, mut uv_bytes) = (0u64, 0u64);
        device.GetCopyableFootprints(&y.res.GetDesc(), 0, 1, 0, Some(&mut footprints[0]), None, None, Some(&mut y_bytes));
        let uv_at = y_bytes.div_ceil(512) * 512;
        device.GetCopyableFootprints(&uv.res.GetDesc(), 0, 1, uv_at, Some(&mut footprints[1]), None, None, Some(&mut uv_bytes));
        let heap_up = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() };
        let buf_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: uv_at + uv_bytes,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut upload: Option<ID3D12Resource> = None;
        device.CreateCommittedResource(&heap_up, D3D12_HEAP_FLAG_NONE, &buf_desc, D3D12_RESOURCE_STATE_GENERIC_READ, None, &mut upload)?;
        let upload = upload.ok_or_else(|| anyhow!("no upload buffer"))?;
        let mut upload_ptr: *mut c_void = null_mut();
        upload.Map(0, None, Some(&mut upload_ptr))?;

        let mut g = Gfx {
            device: device.clone(),
            queue: queue.clone(),
            alloc,
            list,
            fence,
            fence_value: 0,
            fence_event,
            rtv_heap,
            rtv_inc,
            heap,
            heap_inc,
            root,
            pso_nv12,
            cs,
            y,
            uv,
            upload,
            upload_ptr: upload_ptr as *mut u8,
            footprints,
            frame,
            luma,
            mvc,
            mvb,
            mvm,
            mv,
            depth,
            w,
            h,
        };
        for i in 0..2 {
            device.CreateRenderTargetView(&g.frame[i].res, None, g.rtv(i));
            device.CreateShaderResourceView(&g.frame[i].res, None, g.cpu(SRV_FRAME + i as u32));
            device.CreateShaderResourceView(&g.luma[i].res, None, g.cpu(SRV_LUMA + i as u32));
            device.CreateUnorderedAccessView(&g.luma[i].res, None::<&ID3D12Resource>, None, g.cpu(UAV_LUMA + i as u32));
        }
        device.CreateRenderTargetView(&g.depth.res, None, g.rtv(2));
        device.CreateShaderResourceView(&g.y.res, None, g.cpu(SRV_Y));
        device.CreateShaderResourceView(&g.uv.res, None, g.cpu(SRV_UV));
        for (t, srv, uav) in [(&g.mvc, SRV_MVC, UAV_MVC), (&g.mvb, SRV_MVB, UAV_MVB), (&g.mvm, SRV_MVM, UAV_MVM)] {
            device.CreateShaderResourceView(&t.res, None, g.cpu(srv));
            device.CreateUnorderedAccessView(&t.res, None::<&ID3D12Resource>, None, g.cpu(uav));
        }
        device.CreateUnorderedAccessView(&g.mv.res, None::<&ID3D12Resource>, None, g.cpu(UAV_MV));

        // A video has no depth: one constant plane for the whole session. The motion field
        // starts out as "nothing moves" (and stays that way where the generator works out
        // motion by itself).
        g.begin()?;
        g.list.ClearRenderTargetView(g.rtv(2), &[0.5, 0.0, 0.0, 0.0], None);
        to_state(&g.list, &mut g.depth, read);
        g.list.SetDescriptorHeaps(&[Some(g.heap.clone())]);
        g.list.SetComputeRootSignature(&g.root);
        let consts = g.consts();
        g.list.SetComputeRoot32BitConstants(0, (size_of::<CsConsts>() / 4) as u32, &consts as *const _ as *const c_void, 0);
        to_state(&g.list, &mut g.mv, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        g.list.SetPipelineState(&g.cs[5]);
        g.list.SetComputeRootDescriptorTable(5, g.gpu(UAV_MV));
        g.list.Dispatch(w.div_ceil(8), h.div_ceil(8), 1);
        to_state(&g.list, &mut g.mv, read);
        g.submit()?;
        g.wait_idle()?;
        Ok(g)
    }

    fn consts(&self) -> CsConsts {
        CsConsts {
            disp: [self.w as i32, self.h as i32],
            grid: [self.w.div_ceil(8) as i32, self.h.div_ceil(8) as i32],
            q: [self.w.div_ceil(4) as i32, self.h.div_ceil(4) as i32],
            coarse_range: 10,
            ..Default::default()
        }
    }
    unsafe fn rtv(&self, i: usize) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE { ptr: self.rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr + i * self.rtv_inc }
    }
    unsafe fn cpu(&self, slot: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE { ptr: self.heap.GetCPUDescriptorHandleForHeapStart().ptr + (slot * self.heap_inc) as usize }
    }
    unsafe fn gpu(&self, slot: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        D3D12_GPU_DESCRIPTOR_HANDLE { ptr: self.heap.GetGPUDescriptorHandleForHeapStart().ptr + (slot * self.heap_inc) as u64 }
    }
    unsafe fn begin(&mut self) -> Result<()> {
        self.alloc.Reset()?;
        self.list.Reset(&self.alloc, None::<&ID3D12PipelineState>)?;
        Ok(())
    }
    unsafe fn submit(&mut self) -> Result<()> {
        self.list.Close()?;
        self.queue.ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
        Ok(())
    }
    unsafe fn wait_idle(&mut self) -> Result<()> {
        self.fence_value += 1;
        self.queue.Signal(&self.fence, self.fence_value)?;
        if self.fence.GetCompletedValue() < self.fence_value {
            self.fence.SetEventOnCompletion(self.fence_value, self.fence_event)?;
            if WaitForSingleObject(self.fence_event, 5000) != WAIT_OBJECT_0 {
                bail!("the graphics card did not finish a frame ({:?})", self.device.GetDeviceRemovedReason());
            }
        }
        Ok(())
    }

    /// Records one picture: upload, convert to RGB, estimate motion against the previous
    /// picture (when asked), copy to the backbuffer. Leaves the list open so the frame
    /// generator can add its own preparation before `submit`.
    unsafe fn record(&mut self, nv12: &[u8], cur: usize, search: bool, backbuffer: &ID3D12Resource) -> Result<()> {
        let (w, h) = (self.w as usize, self.h as usize);
        if nv12.len() < w * h * 3 / 2 {
            bail!("short picture");
        }
        let (fy, fuv) = (self.footprints[0], self.footprints[1]);
        for row in 0..h {
            std::ptr::copy_nonoverlapping(nv12.as_ptr().add(row * w), self.upload_ptr.add(fy.Offset as usize + row * fy.Footprint.RowPitch as usize), w);
        }
        for row in 0..h / 2 {
            std::ptr::copy_nonoverlapping(
                nv12.as_ptr().add(w * h + row * w),
                self.upload_ptr.add(fuv.Offset as usize + row * fuv.Footprint.RowPitch as usize),
                w / 2 * 2,
            );
        }
        self.begin()?;
        let list = self.list.clone();
        let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        let uav = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
        list.SetDescriptorHeaps(&[Some(self.heap.clone())]);
        for (tex, fp) in [(&mut self.y, fy), (&mut self.uv, fuv)] {
            to_state(&list, tex, D3D12_RESOURCE_STATE_COPY_DEST);
            let dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: lend(&tex.res),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: lend(&self.upload),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp },
            };
            list.CopyTextureRegion(&dst, 0, 0, 0, &from, None);
            to_state(&list, tex, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE);
        }
        to_state(&list, &mut self.frame[cur], D3D12_RESOURCE_STATE_RENDER_TARGET);
        let rtv = self.rtv(cur);
        list.OMSetRenderTargets(1, Some(&rtv), false, None);
        list.RSSetViewports(&[D3D12_VIEWPORT { Width: self.w as f32, Height: self.h as f32, MaxDepth: 1.0, ..Default::default() }]);
        list.RSSetScissorRects(&[RECT { left: 0, top: 0, right: self.w as i32, bottom: self.h as i32 }]);
        list.SetGraphicsRootSignature(&self.root);
        list.SetPipelineState(&self.pso_nv12);
        list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        list.SetGraphicsRootDescriptorTable(1, self.gpu(SRV_Y));
        list.SetGraphicsRootDescriptorTable(2, self.gpu(SRV_UV));
        list.DrawInstanced(3, 1, 0, 0);
        to_state(&list, &mut self.frame[cur], read);

        if search {
            let consts = self.consts();
            let (c, p) = (cur as u32, 1 - cur as u32);
            let groups = |n: i32| (n as u32).div_ceil(8);
            list.SetComputeRootSignature(&self.root);
            list.SetComputeRoot32BitConstants(0, (size_of::<CsConsts>() / 4) as u32, &consts as *const _ as *const c_void, 0);
            // (pipeline, inputs, output slot, grid)
            let passes: [(usize, [Option<u32>; 3], u32, [i32; 2]); 5] = [
                (0, [Some(SRV_FRAME + c), None, None], UAV_LUMA + c, consts.q),
                (1, [Some(SRV_LUMA + c), Some(SRV_LUMA + p), None], UAV_MVC, consts.grid),
                (2, [Some(SRV_FRAME + c), Some(SRV_FRAME + p), Some(SRV_MVC)], UAV_MVB, consts.grid),
                (3, [Some(SRV_MVB), None, None], UAV_MVM, consts.grid),
                (4, [Some(SRV_FRAME + c), Some(SRV_FRAME + p), Some(SRV_MVM)], UAV_MV, consts.disp),
            ];
            for (i, (pso, inputs, out, grid)) in passes.into_iter().enumerate() {
                let target = match i {
                    0 => &mut self.luma[cur],
                    1 => &mut self.mvc,
                    2 => &mut self.mvb,
                    3 => &mut self.mvm,
                    _ => &mut self.mv,
                };
                to_state(&list, target, uav);
                list.SetPipelineState(&self.cs[pso]);
                for (slot, input) in inputs.iter().enumerate() {
                    if let Some(srv) = input {
                        list.SetComputeRootDescriptorTable(1 + slot as u32, self.gpu(*srv));
                    }
                }
                list.SetComputeRootDescriptorTable(5, self.gpu(out));
                list.Dispatch(groups(grid[0]), groups(grid[1]), 1);
                let target = match i {
                    0 => &mut self.luma[cur],
                    1 => &mut self.mvc,
                    2 => &mut self.mvb,
                    3 => &mut self.mvm,
                    _ => &mut self.mv,
                };
                to_state(&list, target, read);
            }
        }

        to_state(&list, &mut self.frame[cur], D3D12_RESOURCE_STATE_COPY_SOURCE);
        list.ResourceBarrier(&[barrier(backbuffer, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_COPY_DEST)]);
        list.CopyResource(backbuffer, &self.frame[cur].res);
        list.ResourceBarrier(&[barrier(backbuffer, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_PRESENT)]);
        to_state(&list, &mut self.frame[cur], read);
        Ok(())
    }
}

// ----------------------------------------------------------------------------------------
// FSR
// ----------------------------------------------------------------------------------------

struct Fsr {
    create: ffx::CreateContext,
    destroy: ffx::DestroyContext,
    configure: ffx::Configure,
    dispatch: ffx::Dispatch,
    swap_context: *mut c_void,
    /// Boxed so the address handed to the generate callback stays put.
    fg: Box<FsrFg>,
    prepare_type: u64,
}

struct FsrFg {
    dispatch: ffx::Dispatch,
    context: *mut c_void,
}

/// FSR asks us to run the interpolation it has scheduled: hand it straight back.
unsafe extern "C" fn fsr_generate(params: *mut ffx::Header, user: *mut c_void) -> u32 {
    let fg = &*(user as *const FsrFg);
    let mut ctx = fg.context;
    (fg.dispatch)(&mut ctx, params)
}

impl Fsr {
    unsafe fn load(dir: &Path) -> Result<Fsr> {
        // The loader looks for the effect DLL next to itself.
        let module = load_library(&dir.join("amd_fidelityfx_loader_dx12.dll"))?;
        let dispatch: ffx::Dispatch = proc(module, "ffxDispatch")?;
        Ok(Fsr {
            create: proc(module, "ffxCreateContext")?,
            destroy: proc(module, "ffxDestroyContext")?,
            configure: proc(module, "ffxConfigure")?,
            dispatch,
            swap_context: null_mut(),
            fg: Box::new(FsrFg { dispatch, context: null_mut() }),
            prepare_type: ffx::PREPARE_V2,
        })
    }

    /// The frame-pacing swapchain for the window; towards us it is an ordinary swapchain.
    unsafe fn create_swapchain(&mut self, factory: &IDXGIFactory4, queue: &ID3D12CommandQueue, hwnd: HWND, desc: &DXGI_SWAP_CHAIN_DESC1) -> Result<IDXGISwapChain4> {
        let mut out: *mut c_void = null_mut();
        // The descriptors must stay alive as long as the context does.
        let version = Box::leak(Box::new(ffx::CreateVersion {
            header: ffx::Header { ty: ffx::CREATE_SWAPCHAIN_VERSION, next: null_mut() },
            version: ffx::SWAPCHAIN_VERSION,
        }));
        let d = Box::leak(Box::new(ffx::CreateSwapChainForHwnd {
            header: ffx::Header { ty: ffx::CREATE_SWAPCHAIN_FOR_HWND, next: &mut version.header },
            swapchain: &mut out,
            hwnd: hwnd.0,
            desc: Box::leak(Box::new(*desc)) as *const _ as *const c_void,
            fullscreen_desc: null(),
            dxgi_factory: factory.as_raw(),
            game_queue: queue.as_raw(),
        }));
        let mut r = (self.create)(&mut self.swap_context, &mut d.header, null());
        if r != 0 {
            d.header.next = null_mut();
            r = (self.create)(&mut self.swap_context, &mut d.header, null());
        }
        if r != 0 || out.is_null() {
            bail!("FSR could not create its swapchain ({})", ffx::code_name(r));
        }
        Ok(IDXGISwapChain4::from_raw(out))
    }

    unsafe fn create_fg(&mut self, device: &ID3D12Device, w: u32, h: u32) -> Result<()> {
        let version = Box::leak(Box::new(ffx::CreateVersion {
            header: ffx::Header { ty: ffx::CREATE_FG_VERSION, next: null_mut() },
            version: ffx::FG_VERSION,
        }));
        let backend = Box::leak(Box::new(ffx::CreateBackendDx12 {
            header: ffx::Header { ty: ffx::CREATE_BACKEND_DX12, next: &mut version.header },
            device: device.as_raw(),
        }));
        let d = Box::leak(Box::new(ffx::CreateFg {
            header: ffx::Header { ty: ffx::CREATE_FG, next: &mut backend.header },
            flags: 0,
            display_size: ffx::Dim2 { width: w, height: h },
            max_render_size: ffx::Dim2 { width: w, height: h },
            back_buffer_format: ffx::FORMAT_R8G8B8A8_UNORM,
        }));
        let mut r = (self.create)(&mut self.fg.context, &mut d.header, null());
        if r != 0 {
            backend.header.next = null_mut();
            r = (self.create)(&mut self.fg.context, &mut d.header, null());
        }
        if r != 0 {
            bail!("FSR could not start frame generation ({})", ffx::code_name(r));
        }
        Ok(())
    }

    unsafe fn configure_frame(&mut self, swap: &IDXGISwapChain4, enabled: bool, w: u32, h: u32, frame_id: u64) -> u32 {
        let c = ffx::ConfigureFg {
            header: ffx::Header { ty: ffx::CONFIGURE_FG, next: null_mut() },
            swap_chain: swap.as_raw(),
            present_callback: None,
            present_callback_user_context: null_mut(),
            frame_generation_callback: if enabled { Some(fsr_generate) } else { None },
            frame_generation_callback_user_context: &*self.fg as *const FsrFg as *mut c_void,
            frame_generation_enabled: enabled,
            allow_async_workloads: false,
            hudless_color: ffx::Resource::empty(),
            flags: 0,
            only_present_generated: false,
            generation_rect: ffx::Rect { left: 0, top: 0, width: w as i32, height: h as i32 },
            frame_id,
        };
        (self.configure)(&mut self.fg.context, &c.header)
    }

    /// Hands this frame's depth and motion vectors to FSR on our own command list.
    unsafe fn prepare(&mut self, g: &Gfx, frame_id: u64, frame_ms: f32, reset: bool) -> u32 {
        let tex = |res: &ID3D12Resource, format: u32, usage: u32| ffx::Resource {
            resource: res.as_raw(),
            desc: ffx::ResourceDesc { ty: ffx::RESOURCE_TYPE_TEXTURE2D, format, width: g.w, height: g.h, depth: 1, mip_count: 1, flags: 0, usage },
            state: ffx::STATE_COMPUTE_READ,
        };
        let mut p = ffx::PrepareV2 {
            header: ffx::Header { ty: self.prepare_type, next: null_mut() },
            frame_id,
            flags: 0,
            command_list: g.list.as_raw(),
            render_size: ffx::Dim2 { width: g.w, height: g.h },
            jitter_offset: ffx::Float2::default(),
            // Our vectors are in pixels, pointing from this picture to the last.
            motion_vector_scale: ffx::Float2 { x: 1.0, y: 1.0 },
            frame_time_delta: frame_ms,
            reset,
            camera_near: 0.1,
            camera_far: 1000.0,
            camera_fov_angle_vertical: 1.0472,
            view_space_to_meters_factor: 1.0,
            depth: tex(&g.depth.res, ffx::FORMAT_R32_FLOAT, 0),
            motion_vectors: tex(&g.mv.res, ffx::FORMAT_R16G16_FLOAT, ffx::USAGE_UAV),
            camera_position: [0.0; 3],
            camera_up: [0.0, 1.0, 0.0],
            camera_right: [1.0, 0.0, 0.0],
            camera_forward: [0.0, 0.0, 1.0],
        };
        let mut r = (self.dispatch)(&mut self.fg.context, &p.header);
        if (r == 2 || r == 7) && self.prepare_type == ffx::PREPARE_V2 {
            self.prepare_type = ffx::PREPARE_V1;
            p.header.ty = self.prepare_type;
            r = (self.dispatch)(&mut self.fg.context, &p.header);
        }
        r
    }

    unsafe fn wait_for_presents(&mut self) {
        let h = ffx::Header { ty: ffx::SWAPCHAIN_WAIT_FOR_PRESENTS, next: null_mut() };
        (self.dispatch)(&mut self.swap_context, &h);
    }
}

// ----------------------------------------------------------------------------------------
// Streamline (DLSS frame generation)
// ----------------------------------------------------------------------------------------

/// NVIDIA's library can be started once per process and wants to see the device being made,
/// so it, its device and its queue live until XMouse exits.
struct Sl {
    get_new_frame_token: sl::GetNewFrameToken,
    set_constants: sl::SetConstants,
    set_tag_for_frame: sl::SetTagForFrame,
    dlssg_get_state: sl::DlssgGetState,
    dlssg_set_options: sl::DlssgSetOptions,
    reflex_sleep: Option<sl::ReflexSleep>,
    pcl_set_marker: Option<sl::PclSetMarker>,
    factory: IDXGIFactory4,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    viewport: sl::ViewportHandle,
}
unsafe impl Send for Sl {}
unsafe impl Sync for Sl {}

unsafe extern "C" fn sl_log(ty: u32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let s = CStr::from_ptr(msg).to_string_lossy();
    if ty == 0 {
        log::trace!("streamline: {}", s.trim_end());
    } else if !s.contains("Ignoring plugin") {
        log::debug!("streamline: {}", s.trim_end());
    }
}

type CreateFactory2Fn = unsafe extern "system" fn(u32, *const GUID, *mut *mut c_void) -> HRESULT;
type CreateDeviceFn = unsafe extern "system" fn(*mut c_void, D3D_FEATURE_LEVEL, *const GUID, *mut *mut c_void) -> HRESULT;

/// What a refusal means for the person at the PC.
fn sl_reason(r: i32) -> String {
    match sl::result_name(r) {
        "eErrorOSDisabledHWS" => "Windows' \"Hardware-accelerated GPU scheduling\" is switched off (Settings, System, Display, Graphics)".into(),
        "eErrorNoSupportedAdapterFound" | "eErrorAdapterNotSupported" | "eErrorFeatureNotSupported" => "this graphics card can't do it (it needs a GeForce RTX 40 or 50)".into(),
        "eErrorDriverOutOfDate" => "the NVIDIA driver is too old".into(),
        "eErrorOSOutOfDate" => "this Windows version is too old".into(),
        n => format!("NVIDIA's library answered {n}"),
    }
}

fn streamline(dir: &Path) -> Result<&'static Sl> {
    static SL: OnceLock<std::result::Result<Sl, String>> = OnceLock::new();
    SL.get_or_init(|| unsafe { sl_start(dir).map_err(|e| format!("{e:#}")) }).as_ref().map_err(|e| anyhow!("{e}"))
}

unsafe fn sl_start(dir: &Path) -> Result<Sl> {
    let module = load_library(&dir.join("sl.interposer.dll"))?;
    let init: sl::Init = proc(module, "slInit")?;
    let dir_w: &'static [u16] = Box::leak(wide(&dir.to_string_lossy()).into_boxed_slice());
    let paths: &'static [*const u16] = Box::leak(vec![dir_w.as_ptr()].into_boxed_slice());
    let features: &'static [u32] = Box::leak(vec![sl::FEATURE_DLSS_G, sl::FEATURE_REFLEX, sl::FEATURE_PCL].into_boxed_slice());
    let mut pref = sl::Preferences::new();
    pref.paths_to_plugins = paths.as_ptr();
    pref.num_paths_to_plugins = 1;
    pref.log_message_callback = Some(sl_log);
    // No over-the-air updates: nothing is downloaded behind the user's back.
    pref.flags = sl::PREF_DISABLE_CL_STATE_TRACKING | sl::PREF_FRAME_BASED_TAGGING;
    pref.features_to_load = features.as_ptr();
    pref.num_features_to_load = features.len() as u32;
    pref.engine_version = c"1.0".as_ptr();
    pref.project_id = c"6d0f8a52-3c1e-4b7a-9f43-2a7c5e81d0b9".as_ptr();
    let r = init(&pref, sl::SDK_VERSION);
    if r != 0 {
        bail!("DLSS could not start: {}", sl_reason(r));
    }
    // The library's own factory and device entry points: objects made through them are the
    // proxies it needs to see the swapchain and its presents.
    let create_factory: CreateFactory2Fn = proc(module, "CreateDXGIFactory2")?;
    let mut raw: *mut c_void = null_mut();
    create_factory(0, &IDXGIFactory4::IID, &mut raw).ok()?;
    let factory = IDXGIFactory4::from_raw(raw);
    let mut adapter: Option<(IDXGIAdapter1, DXGI_ADAPTER_DESC1)> = None;
    let mut i = 0;
    while let Ok(a) = factory.EnumAdapters1(i) {
        let d = a.GetDesc1()?;
        if d.VendorId == 0x10DE && adapter.as_ref().map_or(true, |(_, b)| d.DedicatedVideoMemory > b.DedicatedVideoMemory) {
            adapter = Some((a, d));
        }
        i += 1;
    }
    let (adapter, desc) = adapter.ok_or_else(|| anyhow!("DLSS needs an NVIDIA graphics card"))?;
    let supported: sl::IsFeatureSupported = proc(module, "slIsFeatureSupported")?;
    let info = sl::AdapterInfo::new(&desc.AdapterLuid as *const LUID as *const u8, size_of::<LUID>() as u32);
    let r = supported(sl::FEATURE_DLSS_G, &info);
    if r != 0 {
        bail!("DLSS frame generation isn't available: {}", sl_reason(r));
    }
    let create_device: CreateDeviceFn = proc(module, "D3D12CreateDevice")?;
    let mut raw: *mut c_void = null_mut();
    create_device(adapter.as_raw(), D3D_FEATURE_LEVEL_12_0, &ID3D12Device::IID, &mut raw).ok()?;
    let device = ID3D12Device::from_raw(raw);
    let get_native: sl::GetNativeInterface = proc(module, "slGetNativeInterface")?;
    let set_device: sl::SetD3DDevice = proc(module, "slSetD3DDevice")?;
    let mut native: *mut c_void = null_mut();
    let r = get_native(device.as_raw(), &mut native);
    set_device(if r == 0 && !native.is_null() { native } else { device.as_raw() });
    let feature_fn: sl::GetFeatureFunction = proc(module, "slGetFeatureFunction")?;
    let get = |feature: u32, name: &CStr| -> Option<*mut c_void> {
        let mut p: *mut c_void = null_mut();
        (feature_fn(feature, name.as_ptr(), &mut p) == 0 && !p.is_null()).then_some(p)
    };
    let need = |p: Option<*mut c_void>| p.ok_or_else(|| anyhow!("DLSS frame generation isn't available on this PC"));
    let dlssg_get_state: sl::DlssgGetState = transmute_copy(&need(get(sl::FEATURE_DLSS_G, c"slDLSSGGetState"))?);
    let dlssg_set_options: sl::DlssgSetOptions = transmute_copy(&need(get(sl::FEATURE_DLSS_G, c"slDLSSGSetOptions"))?);
    // Reflex is mandatory for DLSS frame generation; it only has to be told once.
    if let Some(p) = get(sl::FEATURE_REFLEX, c"slReflexSetOptions") {
        let set: sl::ReflexSetOptions = transmute_copy(&p);
        set(&sl::ReflexOptions::new(sl::REFLEX_LOW_LATENCY));
    }
    let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })?;
    Ok(Sl {
        get_new_frame_token: proc(module, "slGetNewFrameToken")?,
        set_constants: proc(module, "slSetConstants")?,
        set_tag_for_frame: proc(module, "slSetTagForFrame")?,
        dlssg_get_state,
        dlssg_set_options,
        reflex_sleep: get(sl::FEATURE_REFLEX, c"slReflexSleep").map(|p| transmute_copy(&p)),
        pcl_set_marker: get(sl::FEATURE_PCL, c"slPCLSetMarker").map(|p| transmute_copy(&p)),
        factory,
        device,
        queue,
        viewport: sl::ViewportHandle::new(0),
    })
}

/// Constants for a picture with no camera: a fixed valid projection, no camera motion,
/// orthonormal axes (the scene-change detector refuses to run otherwise).
fn sl_constants(w: u32, h: u32, reset: bool) -> sl::Constants {
    let mut c = sl::Constants::new();
    let (near, far, fov) = (0.1f32, 1000.0f32, 1.0472f32);
    let aspect = w as f32 / h as f32;
    let ys = 1.0 / (fov * 0.5).tan();
    let xs = ys / aspect;
    let (a, b) = (far / (far - near), -near * far / (far - near));
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    c.camera_view_to_clip = [[xs, 0.0, 0.0, 0.0], [0.0, ys, 0.0, 0.0], [0.0, 0.0, a, 1.0], [0.0, 0.0, b, 0.0]];
    c.clip_to_camera_view = [[1.0 / xs, 0.0, 0.0, 0.0], [0.0, 1.0 / ys, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0 / b], [0.0, 0.0, 1.0, -a / b]];
    c.clip_to_lens_clip = identity;
    c.clip_to_prev_clip = identity;
    c.prev_clip_to_clip = identity;
    c.jitter_offset = [0.0, 0.0];
    c.mvec_scale = [1.0 / w as f32, 1.0 / h as f32];
    c.camera_pinhole_offset = [0.0, 0.0];
    c.camera_pos = [0.0, 0.0, 0.0];
    c.camera_up = [0.0, 1.0, 0.0];
    c.camera_right = [1.0, 0.0, 0.0];
    c.camera_fwd = [0.0, 0.0, 1.0];
    c.camera_near = near;
    c.camera_far = far;
    c.camera_fov = fov;
    c.camera_aspect_ratio = aspect;
    c.motion_vectors_invalid_value = sl::INVALID_FLOAT;
    c.depth_inverted = sl::FALSE;
    c.camera_motion_included = sl::TRUE;
    c.motion_vectors_3d = sl::FALSE;
    c.reset = if reset { sl::TRUE } else { sl::FALSE };
    c
}

fn dlss_options(w: u32, h: u32, on: bool) -> sl::DlssgOptions {
    let mut o = sl::DlssgOptions::new();
    o.mode = if on { sl::DLSSG_MODE_ON } else { sl::DLSSG_MODE_OFF };
    o.num_back_buffers = 3;
    o.mvec_depth_width = w;
    o.mvec_depth_height = h;
    o.color_width = w;
    o.color_height = h;
    o.color_buffer_format = DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32;
    o.mvec_buffer_format = DXGI_FORMAT_R16G16_FLOAT.0 as u32;
    o.depth_buffer_format = DXGI_FORMAT_R32_FLOAT.0 as u32;
    o
}

// ----------------------------------------------------------------------------------------
// The presenter
// ----------------------------------------------------------------------------------------

/// Sleeps coarsely, then spins: both frame generators pace their output from the spacing
/// of our presents, so those have to leave on an even clock.
fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        if deadline - now > Duration::from_micros(2500) {
            std::thread::sleep(Duration::from_millis(1));
        } else {
            std::hint::spin_loop();
        }
    }
}

unsafe fn present_loop(kind: Kind, dir: &Path, hwnd: HWND, w: u32, h: u32, rx: mpsc::Receiver<Cmd>, shared: &Shared) -> Result<()> {
    // Only libraries their makers signed are loaded.
    let (files, signer) = kind.files();
    for f in files {
        install::verify_signature(&dir.join(f), signer)?;
    }
    let mut fsr = None;
    let sl = if kind == Kind::Dlss { Some(streamline(dir)?) } else { None };
    let (factory, device, queue) = match sl {
        Some(s) => (s.factory.clone(), s.device.clone(), s.queue.clone()),
        None => {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let mut adapter: Option<(IDXGIAdapter1, usize)> = None;
            let mut i = 0;
            while let Ok(a) = factory.EnumAdapters1(i) {
                let d = a.GetDesc1()?;
                if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 && adapter.as_ref().map_or(true, |(_, m)| d.DedicatedVideoMemory > *m) {
                    adapter = Some((a, d.DedicatedVideoMemory));
                }
                i += 1;
            }
            let adapter = adapter.ok_or_else(|| anyhow!("no graphics card for Direct3D 12"))?.0;
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).context("this graphics card can't run Direct3D 12")?;
            let device = device.ok_or_else(|| anyhow!("no Direct3D 12 device"))?;
            let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })?;
            (factory, device, queue)
        }
    };
    let mut tearing = BOOL(0);
    if let Ok(f5) = factory.cast::<IDXGIFactory5>() {
        let _ = f5.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut tearing as *mut BOOL as *mut c_void, 4);
    }
    let tearing = tearing.as_bool();
    // The stream's own size, stretched by the system to whatever size the window has.
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w,
        Height: h,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 3,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: if tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
        ..Default::default()
    };
    let swap: IDXGISwapChain4 = if kind == Kind::Fsr {
        let mut f = Fsr::load(dir)?;
        let s = f.create_swapchain(&factory, &queue, hwnd, &desc)?;
        fsr = Some(f);
        s
    } else {
        factory.CreateSwapChainForHwnd(&queue, hwnd, &desc, None, None::<&IDXGIOutput>).context("swapchain")?.cast()?
    };
    let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
    let mut backbuffers: Vec<ID3D12Resource> = Vec::new();
    for i in 0..desc.BufferCount {
        backbuffers.push(swap.GetBuffer(i)?);
    }
    let mut gfx = Gfx::new(&device, &queue, w, h)?;
    if let Some(f) = &mut fsr {
        f.create_fg(&device, w, h)?;
    }
    if let Some(s) = sl {
        let r = (s.dlssg_set_options)(&s.viewport, &dlss_options(w, h, true));
        if r != 0 {
            bail!("DLSS frame generation could not be switched on: {}", sl_reason(r));
        }
    }
    log::info!("{} frame generation: surface {}x{} ready (tearing {tearing})", kind.name(), w, h);

    // Frame numbers never repeat within the process (Streamline keys its state on them).
    static FRAME_NO: AtomicU32 = AtomicU32::new(1);
    let mut first = true;
    let mut last_due: Option<Instant> = None;
    let mut last_present: Option<Instant> = None;
    let mut pending: std::collections::VecDeque<Frame> = Default::default();
    let mut result = Ok(());
    'run: loop {
        if pending.is_empty() {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Cmd::Frame(f)) => pending.push_back(f),
                Ok(Cmd::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
        loop {
            match rx.try_recv() {
                Ok(Cmd::Frame(f)) => pending.push_back(f),
                Ok(Cmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => break 'run,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        // Falling behind: show the newest pictures only.
        while pending.len() > 2 {
            pending.pop_front();
        }
        let Some(f) = pending.pop_front() else { continue };
        if f.width as u32 != w || f.height as u32 != h {
            continue; // the app starts a new surface for a new stream size
        }
        let k = FRAME_NO.fetch_add(1, Ordering::Relaxed);
        let mut token: *mut c_void = null_mut();
        if let Some(s) = sl {
            (s.get_new_frame_token)(&mut token, &k);
            if let (Some(sleep), false) = (s.reflex_sleep, token.is_null()) {
                sleep(token);
            }
        }
        // A little after it arrived, and never much closer to the previous one than the
        // game's own rhythm: network bunching must not reach the frame generator.
        let interval = Duration::from_secs_f64(f.interval.clamp(1.0 / 240.0, 0.1));
        let mut due = f.at + Duration::from_millis(5);
        if let Some(prev) = last_due {
            due = due.max(prev + interval.mul_f64(0.8));
        }
        let now = Instant::now();
        if due > now + interval * 3 {
            due = now; // a stale schedule after a pause
        }
        wait_until(due);
        last_due = Some(due.max(now));

        let marker = |m: u32| {
            if let (Some(s), false) = (sl, token.is_null()) {
                if let Some(set) = s.pcl_set_marker {
                    set(m, token);
                }
            }
        };
        marker(sl::PCL_SIMULATION_START);
        marker(sl::PCL_SIMULATION_END);
        marker(sl::PCL_RENDER_SUBMIT_START);
        let cur = (k % 2) as usize;
        let reset = first || f.cut;
        let bb = swap.GetCurrentBackBufferIndex() as usize;
        let step = (|| -> Result<()> {
            // DLSS works out motion by itself and did better that way in our tests; FSR
            // needs our vectors.
            gfx.record(&f.nv12, cur, kind == Kind::Fsr && !reset, &backbuffers[bb])?;
            if let Some(fsr) = &mut fsr {
                let r = fsr.configure_frame(&swap, true, w, h, k as u64);
                if r != 0 {
                    bail!("FSR refused the frame ({})", ffx::code_name(r));
                }
                let r = fsr.prepare(&gfx, k as u64, (f.interval * 1000.0) as f32, reset);
                if r != 0 {
                    bail!("FSR refused the frame's motion data ({})", ffx::code_name(r));
                }
            }
            if let (Some(s), false) = (sl, token.is_null()) {
                (s.set_constants)(&sl_constants(w, h, reset), token, &s.viewport);
                let state = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0 as u32;
                let mut depth = sl::Resource::tex2d(gfx.depth.res.as_raw(), state, w, h, DXGI_FORMAT_R32_FLOAT.0 as u32);
                let mut mvec = sl::Resource::tex2d(gfx.mv.res.as_raw(), state, w, h, DXGI_FORMAT_R16G16_FLOAT.0 as u32);
                let tags = [
                    sl::ResourceTag::new(&mut depth, sl::BUFFER_DEPTH, sl::LIFECYCLE_VALID_UNTIL_PRESENT),
                    sl::ResourceTag::new(&mut mvec, sl::BUFFER_MOTION_VECTORS, sl::LIFECYCLE_VALID_UNTIL_PRESENT),
                ];
                (s.set_tag_for_frame)(token, &s.viewport, tags.as_ptr(), 2, null_mut());
            }
            gfx.submit()?;
            marker(sl::PCL_RENDER_SUBMIT_END);
            marker(sl::PCL_PRESENT_START);
            swap.Present(0, if tearing { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) }).ok().context("present")?;
            marker(sl::PCL_PRESENT_END);
            gfx.wait_idle()
        })();
        if let Err(e) = step {
            result = Err(e);
            break;
        }
        first = false;
        let at = Instant::now();
        if let Some(prev) = last_present {
            shared.worst_gap.fetch_max(((at - prev).as_secs_f64() * 10_000.0) as u32, Ordering::Relaxed);
        }
        last_present = Some(at);
        shared.real.fetch_add(1, Ordering::Relaxed);
        let shown = match sl {
            Some(s) => {
                let mut st = sl::DlssgState::new();
                (s.dlssg_get_state)(&s.viewport, &mut st, null());
                st.num_frames_actually_presented.max(1) as u64
            }
            // FSR has no counter; its swapchain's present count is read below.
            None => 0,
        };
        if kind == Kind::Fsr {
            shared.shown.store(swap.GetLastPresentCount().unwrap_or(0) as u64, Ordering::Relaxed);
        } else {
            shared.shown.fetch_add(shown, Ordering::Relaxed);
        }
        shared.ready.store(true, Ordering::Relaxed);
    }

    // Let go in the order the vendors ask for: generation off, graphics card idle, our
    // references to the backbuffers, the swapchain, then the generator's own state.
    let _ = gfx.wait_idle();
    if let Some(mut f) = fsr.take() {
        f.configure_frame(&swap, false, w, h, FRAME_NO.fetch_add(1, Ordering::Relaxed) as u64);
        f.wait_for_presents();
        let mut ctx = f.fg.context;
        (f.destroy)(&mut ctx, null());
        backbuffers.clear();
        drop(swap);
        let mut sctx = f.swap_context;
        (f.destroy)(&mut sctx, null());
    } else {
        if let Some(s) = sl {
            (s.dlssg_set_options)(&s.viewport, &dlss_options(w, h, false));
        }
        let _ = gfx.wait_idle();
        backbuffers.clear();
        drop(swap);
    }
    drop(gfx);
    result
}
