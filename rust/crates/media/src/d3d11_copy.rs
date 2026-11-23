//! A D3D11VA picture copied into memory while the processor sleeps until the card is done, instead of FFmpeg's copy, whose Map some drivers wait out by spinning a core.

use crate::d3d11_import::{frame_parts, DeviceContext};
use ffmpeg_sys_next as ff;
use std::ffi::c_void;
use std::ptr;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// The card's own pieces for copying pictures of one decoder: a staging texture, a fence and the event its waits end on.
#[derive(Default)]
pub(crate) struct Copier {
    device: usize,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    fence: Option<(ID3D11Fence, ID3D11DeviceContext4)>,
    event: Option<HANDLE>,
    value: u64,
    failed: bool,
    // Experiment only: the video processor scales each picture to this size before it comes off the card
    fit: Option<Fit>,
    fit_checked: bool,
}

struct Fit {
    size: (u32, u32),
    made: Option<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor, ID3D11Texture2D, ID3D11VideoProcessorOutputView, (u32, u32))>,
    views: Vec<(usize, u32, ID3D11VideoProcessorInputView)>,
}

// SAFETY: the D3D11 objects are free-threaded and used under FFmpeg's device lock; the event handle is only waited on by the decoder's thread.
unsafe impl Send for Copier {}

impl Drop for Copier {
    fn drop(&mut self) {
        if let Some(event) = self.event.take() {
            // SAFETY: the event was made by CreateEventW and is closed once.
            let _ = unsafe { CloseHandle(event) };
        }
    }
}

/// FFmpeg's lock on the device context, released when dropped.
struct Locked<'a>(&'a DeviceContext);

impl<'a> Locked<'a> {
    fn new(d3d: &'a DeviceContext) -> Self {
        // SAFETY: FFmpeg's own lock functions for this device context.
        if let Some(lock) = d3d.lock {
            unsafe { lock(d3d.lock_ctx) }
        }
        Locked(d3d)
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs with the lock taken in new.
        if let Some(unlock) = self.0.unlock {
            unsafe { unlock(self.0.lock_ctx) }
        }
    }
}

impl Copier {
    /// False once it failed, or when TELESCOPE_FFMPEG_COPY asks for FFmpeg's copy.
    pub(crate) fn usable(&self) -> bool {
        !self.failed && std::env::var_os("TELESCOPE_FFMPEG_COPY").is_none()
    }

    /// The picture in `frame` (a D3D11VA surface) as NV12 in `out`; on failure the copier stays off for this decoder.
    pub(crate) unsafe fn copy(&mut self, frame: *const ff::AVFrame, out: *mut ff::AVFrame) -> Result<(), String> {
        // SAFETY: the caller owns both live frames for this call.
        let result = unsafe { self.try_copy(frame, out) };
        self.failed |= result.is_err();
        result
    }

    unsafe fn try_copy(&mut self, frame: *const ff::AVFrame, out: *mut ff::AVFrame) -> Result<(), String> {
        // SAFETY: frame is a live D3D11VA frame; everything taken from it lives as long as its device context.
        unsafe {
            let (d3d, texture, index) = frame_parts(frame)?;
            let device = ID3D11Device::from_raw_borrowed(&d3d.device).ok_or("no D3D11 device")?;
            let context = ID3D11DeviceContext::from_raw_borrowed(&d3d.device_context).ok_or("no D3D11 context")?;
            let source = ID3D11Resource::from_raw_borrowed(&texture).ok_or("no decoder texture")?;
            if self.device != d3d.device as usize {
                self.device = d3d.device as usize;
                self.staging = None;
                self.fence = None;
                self.fit = None;
                self.fit_checked = false;
            }
            if !self.fit_checked {
                self.fit_checked = true;
                self.fit = std::env::var("TELESCOPE_VP_FIT").ok().and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some(Fit { size: (w.parse().ok()?, h.parse().ok()?), made: None, views: Vec::new() })
                });
            }
            if self.fence.is_none() {
                let mut fence: Option<ID3D11Fence> = None;
                device.cast::<ID3D11Device5>().map_err(|e| e.to_string())?.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence).map_err(|e| e.to_string())?;
                let context4 = context.cast::<ID3D11DeviceContext4>().map_err(|e| e.to_string())?;
                self.fence = Some((fence.ok_or("no fence")?, context4));
            }
            if self.event.is_none() {
                self.event = Some(CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?);
            }
            let (pw, ph) = ((*frame).width as u32, (*frame).height as u32);
            let (w, h) = self.fit.as_ref().map_or((pw, ph), |f| f.size);
            if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
                return Err(format!("{w}x{h} can't be copied as NV12"));
            }
            if self.staging.as_ref().is_none_or(|s| (s.1, s.2) != (w, h)) {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_NV12, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0,
                };
                let mut made = None;
                device.CreateTexture2D(&desc, None, Some(&mut made)).map_err(|e| e.to_string())?;
                self.staging = Some((made.ok_or("no staging texture")?, w, h));
            }
            let staging = self.staging.as_ref().map(|s| s.0.clone()).ok_or("no staging texture")?;
            let (fence, context4) = self.fence.clone().ok_or("no fence")?;
            let event = self.event.ok_or("no event")?;
            self.value += 1;
            {
                let _locked = Locked::new(d3d);
                match self.fit.as_mut() {
                    Some(fit) => {
                        let scaled = fit.blt(d3d, device, texture, index, (pw, ph))?;
                        context.CopyResource(&staging, &scaled);
                    }
                    None => {
                        let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
                        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, source, index, Some(&region));
                    }
                }
                context4.Signal(&fence, self.value).map_err(|e| e.to_string())?;
                context.Flush();
            }
            fence.SetEventOnCompletion(self.value, event).map_err(|e| e.to_string())?;
            if WaitForSingleObject(event, 2000) != WAIT_OBJECT_0 {
                return Err("the card took over 2 s to finish a picture".into());
            }
            let _locked = Locked::new(d3d);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| e.to_string())?;
            ff::av_frame_unref(out);
            (*out).format = ff::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
            (*out).width = w as i32;
            (*out).height = h as i32;
            if ff::av_frame_get_buffer(out, 0) < 0 {
                context.Unmap(&staging, 0);
                return Err("out of memory for a picture".into());
            }
            let base = mapped.pData as *const u8;
            let pitch = mapped.RowPitch as usize;
            for (plane, rows, from) in [(0usize, h as usize, 0usize), (1, h as usize / 2, pitch * h as usize)] {
                let dst = (*out).data[plane];
                let stride = (*out).linesize[plane] as usize;
                for row in 0..rows {
                    ptr::copy_nonoverlapping(base.add(from + row * pitch), dst.add(row * stride), w as usize);
                }
            }
            context.Unmap(&staging, 0);
            Ok(())
        }
    }
}

impl Fit {
    /// Experiment: the picture scaled to `size` by the video processor, BT.709 kept, the driver's own enhancements off.
    unsafe fn blt(&mut self, d3d: &DeviceContext, device: &ID3D11Device, texture: *mut c_void, index: u32, (pw, ph): (u32, u32)) -> Result<ID3D11Texture2D, String> {
        // SAFETY: called under FFmpeg's device lock with a live decoder texture.
        unsafe {
            let video: ID3D11VideoDevice = device.cast().map_err(|e| e.to_string())?;
            let vcontext = ID3D11DeviceContext::from_raw_borrowed(&d3d.device_context).ok_or("no context")?.cast::<ID3D11VideoContext>().map_err(|e| e.to_string())?;
            let (ow, oh) = self.size;
            if self.made.as_ref().is_none_or(|m| m.4 != (pw, ph)) {
                self.views.clear();
                let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                    InputWidth: pw, InputHeight: ph,
                    OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                    OutputWidth: ow, OutputHeight: oh,
                    Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                };
                let enumerator = video.CreateVideoProcessorEnumerator(&desc).map_err(|e| e.to_string())?;
                let processor = video.CreateVideoProcessor(&enumerator, 0).map_err(|e| e.to_string())?;
                let studio709 = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
                vcontext.VideoProcessorSetStreamColorSpace(&processor, 0, &studio709);
                vcontext.VideoProcessorSetOutputColorSpace(&processor, &studio709);
                vcontext.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
                vcontext.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
                let src = RECT { left: 0, top: 0, right: pw as i32, bottom: ph as i32 };
                let dst = RECT { left: 0, top: 0, right: ow as i32, bottom: oh as i32 };
                vcontext.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
                vcontext.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
                vcontext.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));
                let out_desc = D3D11_TEXTURE2D_DESC {
                    Width: ow, Height: oh, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_NV12, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32, CPUAccessFlags: 0, MiscFlags: 0,
                };
                let mut target = None;
                device.CreateTexture2D(&out_desc, None, Some(&mut target)).map_err(|e| e.to_string())?;
                let target = target.ok_or("no output texture")?;
                let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut view = None;
                video.CreateVideoProcessorOutputView(&target, &enumerator, &view_desc, Some(&mut view)).map_err(|e| e.to_string())?;
                self.made = Some((enumerator, processor, target, view.ok_or("no output view")?, (pw, ph)));
            }
            let (enumerator, processor, target, output, _) = self.made.as_ref().ok_or("no video processor")?;
            let key = texture as usize;
            let input = match self.views.iter().find(|(k, i, _)| *k == key && *i == index) {
                Some((_, _, v)) => v.clone(),
                None => {
                    let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: index } },
                    };
                    let resource = ID3D11Resource::from_raw_borrowed(&texture).ok_or("no decoder texture")?;
                    let mut view = None;
                    video.CreateVideoProcessorInputView(resource, enumerator, &desc, Some(&mut view)).map_err(|e| e.to_string())?;
                    let view = view.ok_or("no input view")?;
                    self.views.push((key, index, view.clone()));
                    view
                }
            };
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(input)),
                ppFutureSurfaces: ptr::null_mut(),
                ppPastSurfacesRight: ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: ptr::null_mut(),
            };
            let mut streams = [stream];
            let r = vcontext.VideoProcessorBlt(processor, output, 0, &streams);
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            r.map_err(|e| e.to_string())?;
            Ok(target.clone())
        }
    }
}
