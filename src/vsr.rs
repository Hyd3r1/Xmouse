//! PC Play upscaling with NVIDIA RTX Video Super Resolution (Windows).
//!
//! Each decoded NV12 frame is uploaded to Direct3D 11 and scaled to the picture window's pixel
//! size by the D3D11 video processor, with NVIDIA's VSR stream extension switched on (the same
//! call Chromium and VLC make). The result is shared with egui's OpenGL renderer through
//! `WGL_NV_DX_interop2`, so it never goes back through the CPU.
//!
//! VSR also needs "RTX Video Enhancement → Super Resolution" switched on in the NVIDIA app; the
//! app shows "Status: Active" while XMouse uses it. Without NVIDIA support the video processor
//! still scales (plain, like before), and if anything here fails the caller falls back to the
//! CPU path.
//!
//! Everything runs on the UI thread, with egui's GL context current.

use std::ffi::{c_void, CString};

use anyhow::{anyhow, bail, Context, Result};
use glow::HasContext;
use windows::core::{Interface, GUID, PCSTR};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::OpenGL::wglGetProcAddress;

/// NVIDIA's video processor stream extension ("PPE interface"), as used by Chromium.
const NV_PPE_GUID: GUID = GUID::from_values(
    0xd43c_e1b3,
    0x1f4b,
    0x48ac,
    [0xba, 0xee, 0xc3, 0xc2, 0x53, 0x75, 0xe6, 0xf7],
);
const NV_EXT_VERSION: u32 = 1;
const NV_EXT_METHOD_VSR: u32 = 2;

#[repr(C)]
struct NvStreamExtension {
    version: u32,
    method: u32,
    enable: u32,
}

const GL_TEXTURE_2D: u32 = 0x0DE1;
const WGL_ACCESS_READ_ONLY_NV: u32 = 0;

type Handle = *mut c_void;
type FnOpen = unsafe extern "system" fn(*mut c_void) -> Handle;
type FnClose = unsafe extern "system" fn(Handle) -> i32;
type FnRegister = unsafe extern "system" fn(Handle, *mut c_void, u32, u32, u32) -> Handle;
type FnUnregister = unsafe extern "system" fn(Handle, Handle) -> i32;
type FnLock = unsafe extern "system" fn(Handle, i32, *mut Handle) -> i32;

struct Wgl {
    close: FnClose,
    register: FnRegister,
    unregister: FnUnregister,
    lock: FnLock,
    unlock: FnLock,
}

fn wgl_fn<T: Copy>(name: &str) -> Result<T> {
    let c = CString::new(name)?;
    let p = unsafe { wglGetProcAddress(PCSTR(c.as_ptr() as *const u8)) }
        .ok_or_else(|| anyhow!("{name} not available (no WGL_NV_DX_interop)"))?;
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&p));
    Ok(unsafe { std::mem::transmute_copy(&p) })
}

struct Input {
    w: u32,
    h: u32,
    bind: u32,
    staging: ID3D11Texture2D,
    texture: ID3D11Texture2D,
}

struct Output {
    w: u32,
    h: u32,
    texture: ID3D11Texture2D,
    gl: glow::Texture,
    object: Handle,
    egui: egui::TextureId,
    locked: bool,
}

struct Processor {
    key: (u32, u32, u32, u32),
    _enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_view: ID3D11VideoProcessorInputView,
    output_view: ID3D11VideoProcessorOutputView,
}

pub struct Vsr {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    wgl: Wgl,
    interop: Handle,
    input: Option<Input>,
    output: Option<Output>,
    proc_: Option<Processor>,
    /// The NVIDIA driver accepted the Super Resolution switch.
    pub vsr_enabled: bool,
    /// Bumped whenever the video processor is (re)built, e.g. on a window resize.
    pub generation: u64,
    /// The current processor scales *up* with Super Resolution on (the driver's AI path).
    pub upscaling: bool,
    /// Bind flags of the NV12 input texture that this driver accepts for a video processor
    /// input view (found by trying; drivers differ).
    input_bind: Option<u32>,
}

/// Size the shared output texture is allocated at up front (see `ensure_output`).
const MAX_OUTPUT: (u32, u32) = (3840, 2160);

/// Bind flag candidates for the video processor's NV12 input texture, in order of preference.
const INPUT_BINDS: [u32; 4] = [
    0x200, // D3D11_BIND_DECODER
    0, 0x20, // D3D11_BIND_RENDER_TARGET
    0x8,  // D3D11_BIND_SHADER_RESOURCE
];

impl Vsr {
    /// Needs egui's GL context to be current (true inside `App::ui`).
    pub fn new() -> Result<Self> {
        let mut device = None;
        let mut ctx = None;
        let mut level = D3D_FEATURE_LEVEL::default();
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut ctx),
            )
            .context("D3D11CreateDevice")?;
        }
        let device = device.context("no D3D11 device")?;
        let ctx = ctx.context("no D3D11 context")?;
        // NVIDIA's Super Resolution runs part of its work on driver threads; without multithread
        // protection the driver crashed (nvwgf2umx.dll) as soon as it started upscaling.
        // Chromium, mpv and FFmpeg all turn this on for their video devices.
        match ctx.cast::<ID3D11Multithread>() {
            Ok(mt) => unsafe {
                let _ = mt.SetMultithreadProtected(true);
            },
            Err(e) => log::warn!("RTX VSR: no ID3D11Multithread ({e}); continuing"),
        }
        let vdev: ID3D11VideoDevice = device.cast().context("no D3D11 video device")?;
        let vctx: ID3D11VideoContext = ctx.cast().context("no D3D11 video context")?;
        let open: FnOpen = wgl_fn("wglDXOpenDeviceNV")?;
        let wgl = Wgl {
            close: wgl_fn("wglDXCloseDeviceNV")?,
            register: wgl_fn("wglDXRegisterObjectNV")?,
            unregister: wgl_fn("wglDXUnregisterObjectNV")?,
            lock: wgl_fn("wglDXLockObjectsNV")?,
            unlock: wgl_fn("wglDXUnlockObjectsNV")?,
        };
        let interop = unsafe { open(device.as_raw()) };
        if interop.is_null() {
            bail!("wglDXOpenDeviceNV failed (GL and D3D11 on different GPUs?)");
        }
        log::info!("RTX VSR: D3D11 device ready (feature level {:#x})", level.0);
        Ok(Self {
            device,
            ctx,
            vdev,
            vctx,
            wgl,
            interop,
            input: None,
            output: None,
            proc_: None,
            vsr_enabled: false,
            generation: 0,
            upscaling: false,
            input_bind: None,
        })
    }

    /// Scale one NV12 frame to `tw`×`th` pixels. Returns the egui texture and the part of it
    /// (UV size) that holds the picture.
    pub fn process(
        &mut self,
        gl: &glow::Context,
        frame: &mut eframe::Frame,
        w: u32,
        h: u32,
        nv12: &[u8],
        tw: u32,
        th: u32,
    ) -> Result<(egui::TextureId, egui::Vec2)> {
        if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
            bail!("unsupported frame size {w}x{h}");
        }
        let (tw, th) = (tw.clamp(16, 7680), th.clamp(16, 4320));
        if self.ensure_input(w, h, self.input_bind.unwrap_or(INPUT_BINDS[0]))? {
            self.proc_ = None; // its input view points at the old texture
        }
        self.ensure_output(gl, frame, tw, th)?;
        // The processor depends only on the stream size and the (fixed) output texture; the
        // window size is just the destination rectangle below, set per frame.
        self.ensure_processor(w, h)?;
        self.write_input(w, h, nv12)?;
        self.upscaling = self.vsr_enabled && (tw > w || th > h);
        let (out_w, out_h, egui_id) = {
            let o = self.output.as_ref().unwrap();
            (o.w, o.h, o.egui)
        };
        let (processor, input_view, output_view) = {
            let p = self.proc_.as_ref().unwrap();
            (
                p.processor.clone(),
                p.input_view.clone(),
                p.output_view.clone(),
            )
        };

        // The shared texture must not be locked by GL while D3D writes it.
        self.unlock();
        unsafe {
            let src = RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            let dst = RECT {
                left: 0,
                top: 0,
                right: tw as i32,
                bottom: th as i32,
            };
            self.vctx
                .VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            self.vctx
                .VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            self.vctx
                .VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let mut streams = [stream];
            let r = self
                .vctx
                .VideoProcessorBlt(&processor, &output_view, 0, &streams);
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            r.context("VideoProcessorBlt")?;
        }
        self.lock()?;
        Ok((
            egui_id,
            egui::vec2(tw as f32 / out_w as f32, th as f32 / out_h as f32),
        ))
    }

    /// The GL texture `process` renders into, for passes that run after the upscaler
    /// (sharpening, colour). It is readable from GL whenever `process` has returned.
    pub fn gl_texture(&self) -> Option<glow::Texture> {
        self.output.as_ref().filter(|o| o.locked).map(|o| o.gl)
    }

    /// (Re)create the NV12 upload textures for `w`×`h` with these bind flags. True if recreated.
    fn ensure_input(&mut self, w: u32, h: u32, bind: u32) -> Result<bool> {
        if self
            .input
            .as_ref()
            .is_some_and(|i| i.w == w && i.h == h && i.bind == bind)
        {
            return Ok(false);
        }
        {
            let mut desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            let mut texture = None;
            unsafe {
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut staging))
                    .context("NV12 staging texture")?;
                desc.Usage = D3D11_USAGE_DEFAULT;
                desc.BindFlags = bind;
                desc.CPUAccessFlags = 0;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut texture))
                    .with_context(|| format!("NV12 texture (bind {bind:#x})"))?;
            }
            self.input = Some(Input {
                w,
                h,
                bind,
                staging: staging.unwrap(),
                texture: texture.unwrap(),
            });
        }
        Ok(true)
    }

    fn write_input(&mut self, w: u32, h: u32, nv12: &[u8]) -> Result<()> {
        if nv12.len() < (w * h * 3 / 2) as usize {
            bail!("short NV12 frame");
        }
        let input = self.input.as_ref().unwrap();
        unsafe {
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx
                .Map(&input.staging, 0, D3D11_MAP_WRITE, 0, Some(&mut map))
                .context("map NV12 staging")?;
            let pitch = map.RowPitch as usize;
            let dst = map.pData as *mut u8;
            let (w, h) = (w as usize, h as usize);
            // Y rows, then the interleaved UV rows (h/2 of them) right after Y's h rows.
            for row in 0..h + h / 2 {
                std::ptr::copy_nonoverlapping(nv12.as_ptr().add(row * w), dst.add(row * pitch), w);
            }
            self.ctx.Unmap(&input.staging, 0);
            self.ctx.CopyResource(&input.texture, &input.staging);
        }
        Ok(())
    }

    fn ensure_output(
        &mut self,
        gl: &glow::Context,
        frame: &mut eframe::Frame,
        tw: u32,
        th: u32,
    ) -> Result<()> {
        if self.output.as_ref().is_some_and(|o| o.w >= tw && o.h >= th) {
            return Ok(());
        }
        // Allocated once, big enough for any normal window (4K), so resizing the window never
        // tears down a texture the upscaler is using: re-creating it on F11 crashed the NVIDIA
        // driver (nvwgf2umx.dll). Only a window larger than 4K grows it.
        let (w, h) = self
            .output
            .as_ref()
            .map(|o| (o.w.max(tw), o.h.max(th)))
            .unwrap_or((tw.max(MAX_OUTPUT.0), th.max(MAX_OUTPUT.1)));
        self.release_output(gl);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .context("output texture")?;
        }
        let texture = texture.unwrap();
        let gl_tex = unsafe { gl.create_texture() }.map_err(|e| anyhow!("GL texture: {e}"))?;
        let object = unsafe {
            (self.wgl.register)(
                self.interop,
                texture.as_raw(),
                gl_tex.0.get(),
                GL_TEXTURE_2D,
                WGL_ACCESS_READ_ONLY_NV,
            )
        };
        if object.is_null() {
            unsafe { gl.delete_texture(gl_tex) };
            bail!("wglDXRegisterObjectNV failed");
        }
        let egui = frame.register_native_glow_texture(gl_tex);
        self.output = Some(Output {
            w,
            h,
            texture,
            gl: gl_tex,
            object,
            egui,
            locked: false,
        });
        self.lock()?;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(gl_tex));
            for (p, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        self.proc_ = None; // its output view points at the old texture
        log::info!("RTX VSR: output texture {w}x{h}");
        Ok(())
    }

    fn ensure_processor(&mut self, w: u32, h: u32) -> Result<()> {
        let (tw, th) = {
            let o = self.output.as_ref().unwrap();
            (o.w, o.h)
        };
        let key = (w, h, tw, th);
        if self.proc_.as_ref().is_some_and(|p| p.key == key) {
            return Ok(());
        }
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: w,
            InputHeight: h,
            OutputFrameRate: rate,
            OutputWidth: tw,
            OutputHeight: th,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        unsafe {
            let enumerator = self
                .vdev
                .CreateVideoProcessorEnumerator(&desc)
                .context("video processor enumerator")?;
            let in_caps = enumerator
                .CheckVideoProcessorFormat(DXGI_FORMAT_NV12)
                .unwrap_or(0);
            if in_caps & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
                bail!("video processor can't take NV12 input");
            }
            let caps = enumerator
                .CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM)
                .unwrap_or(0);
            if caps & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                bail!("video processor can't output RGBA8");
            }
            let processor = self
                .vdev
                .CreateVideoProcessor(&enumerator, 0)
                .context("video processor")?;

            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            // Find bind flags this driver accepts for an input view (remembered afterwards).
            let candidates: Vec<u32> = match self.input_bind {
                Some(b) => vec![b],
                None => INPUT_BINDS.to_vec(),
            };
            let mut input_view = None;
            let mut last_err = None;
            for bind in candidates {
                if let Err(e) = self.ensure_input(w, h, bind) {
                    last_err = Some(e);
                    continue;
                }
                let input = self.input.as_ref().unwrap();
                match self.vdev.CreateVideoProcessorInputView(
                    &input.texture,
                    &enumerator,
                    &in_desc,
                    Some(&mut input_view),
                ) {
                    Ok(()) => {
                        if self.input_bind.is_none() {
                            log::info!("RTX VSR: NV12 input texture bind flags {bind:#x}");
                        }
                        self.input_bind = Some(bind);
                        break;
                    }
                    Err(e) => {
                        log::debug!("RTX VSR: input view with bind {bind:#x}: {e}");
                        last_err = Some(anyhow!("input view (bind {bind:#x}): {e}"));
                    }
                }
            }
            if input_view.is_none() {
                return Err(last_err.unwrap_or_else(|| anyhow!("no input view")));
            }
            let output = self.output.as_ref().unwrap();
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view = None;
            self.vdev
                .CreateVideoProcessorOutputView(
                    &output.texture,
                    &enumerator,
                    &out_desc,
                    Some(&mut output_view),
                )
                .context("output view")?;

            // BT.709, limited range in; full-range RGB out.
            let in_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                _bitfield: (1 << 2) | (1 << 4),
            };
            let out_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            self.vctx
                .VideoProcessorSetStreamColorSpace(&processor, 0, &in_cs);
            self.vctx
                .VideoProcessorSetOutputColorSpace(&processor, &out_cs);
            self.vctx.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            self.vctx
                .VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);

            let ext = NvStreamExtension {
                version: NV_EXT_VERSION,
                method: NV_EXT_METHOD_VSR,
                enable: 1,
            };
            let hr = self.vctx.VideoProcessorSetStreamExtension(
                &processor,
                0,
                &NV_PPE_GUID,
                std::mem::size_of::<NvStreamExtension>() as u32,
                &ext as *const _ as *const c_void,
            );
            self.vsr_enabled = hr >= 0;
            log::info!(
                "RTX VSR: {w}x{h} stream, output up to {tw}x{th}, NVIDIA Super Resolution {} (HRESULT {hr:#x})",
                if self.vsr_enabled {
                    "ON"
                } else {
                    "not available"
                }
            );

            self.generation += 1;
            self.proc_ = Some(Processor {
                key,
                _enumerator: enumerator,
                processor,
                input_view: input_view.unwrap(),
                output_view: output_view.unwrap(),
            });
        }
        Ok(())
    }

    fn lock(&mut self) -> Result<()> {
        if let Some(o) = self.output.as_mut() {
            if !o.locked {
                let mut obj = o.object;
                if unsafe { (self.wgl.lock)(self.interop, 1, &mut obj) } == 0 {
                    bail!("wglDXLockObjectsNV failed");
                }
                o.locked = true;
            }
        }
        Ok(())
    }

    fn unlock(&mut self) {
        if let Some(o) = self.output.as_mut() {
            if o.locked {
                let mut obj = o.object;
                unsafe { (self.wgl.unlock)(self.interop, 1, &mut obj) };
                o.locked = false;
            }
        }
    }

    fn release_output(&mut self, gl: &glow::Context) {
        self.unlock();
        if let Some(o) = self.output.take() {
            unsafe {
                (self.wgl.unregister)(self.interop, o.object);
                // egui keeps the id; it is never drawn again.
                gl.delete_texture(o.gl);
            }
        }
        self.proc_ = None;
    }
}

impl Drop for Vsr {
    fn drop(&mut self) {
        self.unlock();
        self.proc_ = None;
        if let Some(o) = self.output.take() {
            unsafe { (self.wgl.unregister)(self.interop, o.object) };
        }
        unsafe { (self.wgl.close)(self.interop) };
    }
}
