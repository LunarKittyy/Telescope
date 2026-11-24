//! A D3D11VA picture scaled to the camera's size by the card's video processor before it comes down, so a 4K stream costs what a 1080p one does to copy and convert.
//! It runs on FFmpeg's own D3D11 device: nothing is shared with the pipeline's, so it works under any graphics backend.

use crate::d3d11_copy::Locked;
use crate::d3d11_import::{frame_parts, DeviceContext};
use ffmpeg_sys_next as ff;
use std::ffi::c_void;
use std::ptr;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// How long to wait for the card to finish a picture before giving the scaling up.
const WAIT_MS: u32 = 500;

/// Input views kept for the decoder's textures before they are made again, far more than its pool holds.
const MOST_VIEWS: usize = 64;

/// What one pair of sizes needs: the processor, the texture it draws into and the view of it.
struct Made {
    from: (u32, u32),
    to: (u32, u32),
    video: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    target: ID3D11Texture2D,
    output: ID3D11VideoProcessorOutputView,
}

/// A decoder texture (kept alive so its address can't be reused for another while its view is cached), the slice and the view of it.
type InputView = (ID3D11Resource, u32, ID3D11VideoProcessorInputView);

/// The card's own pieces for scaling the pictures of one decoder: the video processor, a staging texture, a fence and the event its waits end on.
#[derive(Default)]
pub(crate) struct Fitter {
    device: usize,
    made: Option<Made>,
    views: Vec<InputView>,
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
    fence: Option<(ID3D11Fence, ID3D11DeviceContext4)>,
    event: Option<HANDLE>,
    // A wait ran out: the card may still signal the event, so it stays open when the fitter goes
    abandoned: bool,
    value: u64,
}

// SAFETY: the D3D11 objects are free-threaded and used under FFmpeg's device lock; the event handle is only waited on by the thread that holds the codec.
unsafe impl Send for Fitter {}

impl Drop for Fitter {
    fn drop(&mut self) {
        if let Some(event) = self.event.take().filter(|_| !self.abandoned) {
            // SAFETY: the event was made by CreateEventW and is closed once.
            let _ = unsafe { CloseHandle(event) };
        }
    }
}

impl Fitter {
    /// The picture in `frame` (a D3D11VA surface) scaled to `size` and copied as NV12 into `out`.
    pub(crate) unsafe fn fit(&mut self, frame: *const ff::AVFrame, out: *mut ff::AVFrame, size: (u32, u32)) -> Result<(), String> {
        // SAFETY: frame is a live D3D11VA frame; everything taken from it lives as long as its device context.
        unsafe {
            let (d3d, texture, index) = frame_parts(frame)?;
            let device = ID3D11Device::from_raw_borrowed(&d3d.device).ok_or("no D3D11 device")?;
            let context = ID3D11DeviceContext::from_raw_borrowed(&d3d.device_context).ok_or("no D3D11 context")?;
            let (w, h) = size;
            if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
                return Err(format!("{w}x{h} can't be scaled to as NV12"));
            }
            if self.device != d3d.device as usize {
                self.device = d3d.device as usize;
                self.made = None;
                self.views.clear();
                self.staging = None;
                self.fence = None;
            }
            if self.fence.is_none() {
                let mut fence: Option<ID3D11Fence> = None;
                device.cast::<ID3D11Device5>().and_then(|d| d.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence)).map_err(|e| e.to_string())?;
                let context4 = context.cast::<ID3D11DeviceContext4>().map_err(|e| e.to_string())?;
                self.fence = Some((fence.ok_or("no fence")?, context4));
            }
            if self.event.is_none() {
                self.event = Some(CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?);
            }
            if self.staging.as_ref().is_none_or(|s| s.1 != size) {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_NV12, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0,
                };
                let mut made = None;
                device.CreateTexture2D(&desc, None, Some(&mut made)).map_err(|e| e.to_string())?;
                self.staging = Some((made.ok_or("no staging texture")?, size));
            }
            let staging = self.staging.as_ref().map(|s| s.0.clone()).ok_or("no staging texture")?;
            let (fence, context4) = self.fence.clone().ok_or("no fence")?;
            let event = self.event.ok_or("no event")?;
            self.value += 1;
            {
                let _locked = Locked::new(d3d);
                let scaled = self.blt(d3d, device, texture, index, ((*frame).width as u32, (*frame).height as u32), size)?;
                context.CopyResource(&staging, &scaled);
                context4.Signal(&fence, self.value).map_err(|e| e.to_string())?;
                context.Flush();
            }
            fence.SetEventOnCompletion(self.value, event).map_err(|e| e.to_string())?;
            if WaitForSingleObject(event, WAIT_MS) != WAIT_OBJECT_0 {
                self.abandoned = true;
                return Err(format!("the card took over {WAIT_MS} ms to scale a picture"));
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

    /// The picture scaled to `to` by the video processor into its own texture, in the colours it came in (BT.709 flagged both ways, so nothing is converted) with the driver's enhancements off.
    unsafe fn blt(&mut self, d3d: &DeviceContext, device: &ID3D11Device, texture: *mut c_void, index: u32, from: (u32, u32), to: (u32, u32)) -> Result<ID3D11Texture2D, String> {
        // SAFETY: called under FFmpeg's device lock with a live decoder texture.
        unsafe {
            if self.made.as_ref().is_none_or(|m| m.from != from || m.to != to) {
                self.views.clear();
                self.made = None;
                self.made = Some(Self::make(d3d, device, from, to)?);
            }
            let made = self.made.as_ref().ok_or("no video processor")?;
            let resource = ID3D11Resource::from_raw_borrowed(&texture).ok_or("no decoder texture")?;
            let input = match self.views.iter().find(|(r, i, _)| r.as_raw() == texture && *i == index) {
                Some((_, _, view)) => view.clone(),
                None => {
                    let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: index } },
                    };
                    let mut view = None;
                    made.video.CreateVideoProcessorInputView(resource, &made.enumerator, &desc, Some(&mut view)).map_err(|e| e.to_string())?;
                    let view = view.ok_or("no input view")?;
                    if self.views.len() >= MOST_VIEWS {
                        self.views.clear();
                    }
                    self.views.push((resource.clone(), index, view.clone()));
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
            let result = made.context.VideoProcessorBlt(&made.processor, &made.output, 0, &streams);
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            result.map_err(|e| e.to_string())?;
            Ok(made.target.clone())
        }
    }

    unsafe fn make(d3d: &DeviceContext, device: &ID3D11Device, from: (u32, u32), to: (u32, u32)) -> Result<Made, String> {
        // SAFETY: called under FFmpeg's device lock.
        unsafe {
            let video: ID3D11VideoDevice = device.cast().map_err(|e| e.to_string())?;
            let context = ID3D11DeviceContext::from_raw_borrowed(&d3d.device_context).ok_or("no context")?.cast::<ID3D11VideoContext>().map_err(|e| e.to_string())?;
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                InputWidth: from.0, InputHeight: from.1,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                OutputWidth: to.0, OutputHeight: to.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = video.CreateVideoProcessorEnumerator(&desc).map_err(|e| e.to_string())?;
            let processor = video.CreateVideoProcessor(&enumerator, 0).map_err(|e| e.to_string())?;
            // BT.709, 16 to 235 on both sides: the scaler does no colour conversion, the picture's own matrix is applied later on the processor
            let studio709 = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
            context.VideoProcessorSetStreamColorSpace(&processor, 0, &studio709);
            context.VideoProcessorSetOutputColorSpace(&processor, &studio709);
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            let src = RECT { left: 0, top: 0, right: from.0 as i32, bottom: from.1 as i32 };
            let dst = RECT { left: 0, top: 0, right: to.0 as i32, bottom: to.1 as i32 };
            context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));
            let out_desc = D3D11_TEXTURE2D_DESC {
                Width: to.0, Height: to.1, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_NV12, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
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
            Ok(Made { from, to, video, context, enumerator, processor, target, output: view.ok_or("no output view")? })
        }
    }
}
