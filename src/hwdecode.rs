//! GPU video decoding (Windows): Windows' own H.264 decoder (Media Foundation) running on the
//! graphics card through D3D11, in low-latency mode so every frame comes out as soon as it's
//! decoded. Frames are handed over as NV12, the format the RTX upscaler takes.
//!
//! Everything here runs on the video thread that created it (COM objects stay on that thread).

use std::mem::ManuallyDrop;

use anyhow::{bail, Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

/// One decoded picture: the Y plane (width × height) followed by interleaved UV (half height).
pub struct Nv12Frame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// 60 fps in Media Foundation's 100 ns units (timestamps only keep the decoder happy).
const FRAME_TIME: i64 = 166_667;

pub struct HwDecoder {
    mft: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    /// CPU-readable copy of the decoder's output texture.
    staging: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
    /// Visible picture size (the decoder's textures can be taller, e.g. 1088 for 1080).
    size: (usize, usize),
    /// Coded size and row stride, for the (unexpected) system-memory output path.
    coded: (usize, usize),
    stride: usize,
    provides_samples: bool,
    sample_size: u32,
    time: i64,
}

enum Pulled {
    Frame(Option<Nv12Frame>),
    NeedInput,
    Changed,
}

impl HwDecoder {
    pub fn new() -> Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup")?;

            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .context("D3D11CreateDevice")?;
            let device: ID3D11Device = device.context("no D3D11 device")?;
            let ctx: ID3D11DeviceContext = ctx.context("no D3D11 context")?;
            // Media Foundation uses the device from its own threads.
            if let Ok(mt) = ctx.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).context("MFCreateDXGIDeviceManager")?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&device, token).context("DXGI device manager ResetDevice")?;

            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .context("Windows H.264 decoder")?;
            let attrs = mft.GetAttributes().context("decoder attributes")?;
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                bail!("the H.264 decoder can't use D3D11");
            }
            // Output each frame the moment it's decoded instead of queueing a few.
            attrs.SetUINT32(&MF_LOW_LATENCY, 1).context("low-latency mode")?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("hand the GPU to the decoder")?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            mft.SetInputType(0, &input, 0).context("H.264 input type")?;

            let mut me = Self {
                mft,
                _manager: manager,
                device,
                ctx,
                staging: None,
                size: (0, 0),
                coded: (0, 0),
                stride: 0,
                provides_samples: true,
                sample_size: 0,
                time: 0,
            };
            // The output type may only be known after the first frame (stream change).
            if let Err(e) = me.set_output_type() {
                log::debug!("GPU decoder: output type later ({e:#})");
            }
            me.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            me.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(me)
        }
    }

    /// Pick NV12 output and read the picture size.
    fn set_output_type(&mut self) -> Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                let t = match self.mft.GetOutputAvailableType(0, i) {
                    Ok(t) => t,
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => bail!("decoder has no NV12 output"),
                    Err(e) => return Err(e).context("output types"),
                };
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    self.mft.SetOutputType(0, &t, 0).context("NV12 output type")?;
                    break;
                }
                i += 1;
            }
            let t = self.mft.GetOutputCurrentType(0)?;
            let packed = t.GetUINT64(&MF_MT_FRAME_SIZE).context("frame size")?;
            let coded = ((packed >> 32) as usize, (packed & 0xffff_ffff) as usize);
            let mut size = coded;
            let mut area = MFVideoArea::default();
            let bytes = std::slice::from_raw_parts_mut(
                &mut area as *mut MFVideoArea as *mut u8,
                std::mem::size_of::<MFVideoArea>(),
            );
            if t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).is_ok()
                && area.Area.cx > 0
                && area.Area.cy > 0
            {
                size = (area.Area.cx as usize, area.Area.cy as usize);
            }
            // NV12 stores colour as U,V pairs, so an odd size has no complete last pair (the
            // RGBA conversion would read past the row). Drop the odd last column/row instead.
            size = (size.0 & !1, size.1 & !1);
            self.coded = coded;
            self.size = size;
            self.stride = t
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|s| (s as i32).unsigned_abs() as usize)
                .unwrap_or(coded.0);
            let info = self.mft.GetOutputStreamInfo(0)?;
            self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            self.sample_size = info.cbSize;
            self.staging = None;
            log::info!(
                "GPU decoder: {}x{} (coded {}x{}), {}",
                size.0,
                size.1,
                coded.0,
                coded.1,
                if self.provides_samples { "GPU textures" } else { "system memory" }
            );
        }
        Ok(())
    }

    /// Decode one access unit (Annex B). Returns the newest decoded picture if `want` is set;
    /// otherwise the picture is decoded (later frames need it) but not copied out.
    pub fn decode(&mut self, au: &[u8], want: bool) -> Result<Option<Nv12Frame>> {
        let sample = unsafe {
            let buf = MFCreateMemoryBuffer(au.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), p, au.len());
            buf.Unlock()?;
            buf.SetCurrentLength(au.len() as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(self.time)?;
            s.SetSampleDuration(FRAME_TIME)?;
            self.time += FRAME_TIME;
            s
        };
        let mut newest = None;
        let mut fed = false;
        for _ in 0..2 {
            match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
                Ok(()) => {
                    fed = true;
                    break;
                }
                // Full: take the pictures out first, then feed again.
                Err(e) if e.code() == MF_E_NOTACCEPTING => self.drain(want, &mut newest)?,
                Err(e) => return Err(e).context("decoder input"),
            }
        }
        if !fed {
            bail!("decoder won't take input");
        }
        self.drain(want, &mut newest)?;
        Ok(newest)
    }

    fn drain(&mut self, want: bool, newest: &mut Option<Nv12Frame>) -> Result<()> {
        // One access unit gives a picture or two; the limit only keeps a misbehaving decoder
        // (one that never asks for more input) from hanging the video thread.
        for _ in 0..64 {
            match self.pull(want)? {
                Pulled::Frame(Some(f)) => *newest = Some(f),
                Pulled::Frame(None) | Pulled::Changed => {}
                Pulled::NeedInput => return Ok(()),
            }
        }
        bail!("decoder never asked for more input");
    }

    fn pull(&mut self, want: bool) -> Result<Pulled> {
        unsafe {
            let ours = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.sample_size.max(1))?)?;
                Some(s)
            };
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(ours),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, std::slice::from_mut(&mut out), &mut status);
            let sample = ManuallyDrop::take(&mut out.pSample);
            drop(ManuallyDrop::take(&mut out.pEvents));
            match r {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(Pulled::NeedInput),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.set_output_type()?;
                    return Ok(Pulled::Changed);
                }
                Err(e) => return Err(e).context("decoder output"),
            }
            let sample = sample.context("decoder gave no picture")?;
            if !want {
                return Ok(Pulled::Frame(None));
            }
            Ok(Pulled::Frame(Some(self.read(&sample)?)))
        }
    }

    /// Copy a decoded picture to memory as tightly packed NV12.
    fn read(&mut self, sample: &IMFSample) -> Result<Nv12Frame> {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            bail!("picture size unknown");
        }
        let mut data = vec![0u8; w * h + w * h.div_ceil(2)];
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(dx) = buffer.cast::<IMFDXGIBuffer>() {
                let mut raw = std::ptr::null_mut();
                dx.GetResource(&ID3D11Texture2D::IID, &mut raw).context("decoded texture")?;
                if raw.is_null() {
                    bail!("decoder gave no texture");
                }
                // (GetResource added a reference; `tex` owns it and releases it when dropped.)
                let tex = ID3D11Texture2D::from_raw(raw);
                let sub = dx.GetSubresourceIndex()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                tex.GetDesc(&mut desc);
                if desc.Format != DXGI_FORMAT_NV12 {
                    bail!("decoder output isn't NV12 ({:?})", desc.Format);
                }
                // The rows copied below must exist in the texture: never read past it if the
                // announced picture size and the actual texture ever disagree.
                if w > desc.Width as usize || h > desc.Height as usize {
                    bail!("picture {w}x{h} is larger than its texture {}x{}", desc.Width, desc.Height);
                }
                let staging = self.staging_for(&desc)?;
                self.ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, &tex, sub, None);
                let mut map = D3D11_MAPPED_SUBRESOURCE::default();
                self.ctx
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))
                    .context("map decoded picture")?;
                let pitch = map.RowPitch as usize;
                let src = map.pData as *const u8;
                if src.is_null() || pitch < w {
                    self.ctx.Unmap(&staging, 0);
                    bail!("decoded picture can't be read (row pitch {pitch})");
                }
                // Y rows, then UV rows, which start after all of the texture's rows.
                for row in 0..h {
                    std::ptr::copy_nonoverlapping(src.add(row * pitch), data.as_mut_ptr().add(row * w), w);
                }
                let uv = src.add(pitch * desc.Height as usize);
                for row in 0..h.div_ceil(2) {
                    std::ptr::copy_nonoverlapping(
                        uv.add(row * pitch),
                        data.as_mut_ptr().add(w * h + row * w),
                        w,
                    );
                }
                self.ctx.Unmap(&staging, 0);
            } else {
                let mut p = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut p, None, Some(&mut len))?;
                let (stride, ch) = (self.stride.max(w), self.coded.1.max(h));
                let need = stride * ch + stride * ch.div_ceil(2);
                if (len as usize) < need {
                    buffer.Unlock()?;
                    bail!("short decoded picture ({len} < {need})");
                }
                for row in 0..h {
                    std::ptr::copy_nonoverlapping(p.add(row * stride), data.as_mut_ptr().add(row * w), w);
                }
                for row in 0..h.div_ceil(2) {
                    std::ptr::copy_nonoverlapping(
                        p.add(stride * ch + row * stride),
                        data.as_mut_ptr().add(w * h + row * w),
                        w,
                    );
                }
                buffer.Unlock()?;
            }
        }
        Ok(Nv12Frame {
            width: w,
            height: h,
            data,
        })
    }

    fn staging_for(&mut self, src: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
        if let Some((t, d)) = &self.staging {
            if d.Width == src.Width && d.Height == src.Height {
                return Ok(t.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: src.Width,
            Height: src.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut tex))
                .context("staging texture for decoded pictures")?;
        }
        let tex = tex.context("no staging texture")?;
        self.staging = Some((tex.clone(), desc));
        Ok(tex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::encoder::Encoder;
    use openh264::formats::YUVBuffer;

    /// Encode a few frames (dark left half, bright right half) and decode them on the GPU.
    /// Skips (with a note) on machines where GPU decoding can't start.
    #[test]
    fn gpu_decoder_round_trip() {
        let (w, h) = (1280usize, 720usize);
        let mut i420 = vec![128u8; w * h * 3 / 2];
        for row in 0..h {
            for x in 0..w {
                i420[row * w + x] = if x < w / 2 { 60 } else { 200 };
            }
        }
        // Left half red-ish (U=90, V=240), right half neutral: checks the colour planes too.
        let (cw, ch) = (w / 2, h / 2);
        for row in 0..ch {
            for x in 0..cw / 2 {
                i420[w * h + row * cw + x] = 90;
                i420[w * h + cw * ch + row * cw + x] = 240;
            }
        }
        let frame = YUVBuffer::from_vec(i420, w, h);
        let mut enc = Encoder::new().expect("encoder");
        let mut dec = match HwDecoder::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("GPU decoding unavailable here, skipping: {e:#}");
                return;
            }
        };
        let mut last = None;
        for _ in 0..6 {
            let au = enc.encode(&frame).expect("encode").to_vec();
            if let Some(f) = dec.decode(&au, true).expect("GPU decode") {
                last = Some(f);
            }
        }
        let f = last.expect("the GPU decoder produced no picture");
        assert_eq!((f.width, f.height), (w, h), "visible size (not the padded 736)");
        let left = f.data[(h / 2) * w + w / 4] as i32;
        let right = f.data[(h / 2) * w + 3 * w / 4] as i32;
        eprintln!("GPU decoded {}x{}: left Y={left}, right Y={right}", f.width, f.height);
        assert!((left - 60).abs() < 8 && (right - 200).abs() < 8, "picture content");
        // NV12 chroma rows are U,V pairs: sample the middle row, left quarter and right quarter.
        let uv_row = &f.data[w * h + (h / 4) * w..w * h + (h / 4) * w + w];
        let (lu, lv) = (uv_row[w / 4 & !1] as i32, uv_row[(w / 4 & !1) + 1] as i32);
        let (ru, rv) = (uv_row[3 * w / 4 & !1] as i32, uv_row[(3 * w / 4 & !1) + 1] as i32);
        eprintln!("chroma left U={lu} V={lv}, right U={ru} V={rv}");
        assert!((lu - 90).abs() < 10 && (lv - 240).abs() < 10, "left chroma");
        assert!((ru - 128).abs() < 10 && (rv - 128).abs() < 10, "right chroma");
    }
}

impl Drop for HwDecoder {
    fn drop(&mut self) {
        // (Media Foundation stays started: its objects are released after this, and MFStartup
        // is reference counted, so a later session simply starts it again.)
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}
