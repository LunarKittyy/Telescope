//! A D3D11VA picture copied into memory while the processor sleeps until the card is done, instead of FFmpeg's copy, whose Map some drivers wait out by spinning a core.

use crate::d3d11_import::{frame_parts, DeviceContext};
use ffmpeg_sys_next as ff;
use std::ptr;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// PCI vendor number of NVIDIA's cards.
const NVIDIA: u32 = 0x10de;

/// How long to wait for the fence, well under the H.264 stall detector's 500 ms.
const FENCE_WAIT_MS: u32 = 100;

/// The card's own pieces for copying pictures of one decoder: a staging texture, a fence and the event its waits end on.
#[derive(Default)]
pub(crate) struct Copier {
    device: usize,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    fence: Option<(ID3D11Fence, ID3D11DeviceContext4)>,
    event: Option<HANDLE>,
    value: u64,
    failed: bool,
    removed: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    no_fence: bool,
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
pub(crate) struct Locked<'a>(&'a DeviceContext);

impl<'a> Locked<'a> {
    pub(crate) fn new(d3d: &'a DeviceContext) -> Self {
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
    /// Tests: FFmpeg's copy from now on.
    #[cfg(test)]
    pub(crate) fn disable(&mut self) {
        self.failed = true;
    }

    /// True when the last failure came with the card's device removed; FFmpeg's own copy must not run on it.
    pub(crate) fn device_removed(&self) -> bool {
        self.removed
    }

    /// Tests: the driver has no fences at all.
    #[cfg(test)]
    pub(crate) fn fence_unavailable(&self) -> bool {
        self.no_fence
    }

    /// False once it failed for this decoder.
    pub(crate) fn usable(&self) -> bool {
        !self.failed
    }

    /// The picture in `frame` (a D3D11VA surface) as NV12 in `out`, or false for a card this isn't used on; on failure the copier stays off for this decoder.
    pub(crate) unsafe fn copy(&mut self, frame: *const ff::AVFrame, out: *mut ff::AVFrame) -> Result<bool, String> {
        // SAFETY: the caller owns both live frames for this call.
        let result = unsafe { self.try_copy(frame, out) };
        self.failed |= !matches!(result, Ok(true));
        if result.is_err() {
            // SAFETY: as above; the device outlives the frame.
            self.removed = unsafe { frame_parts(frame) }
                .ok()
                .and_then(|(d3d, _, _)| unsafe { ID3D11Device::from_raw_borrowed(&d3d.device) }.map(|d| unsafe { d.GetDeviceRemovedReason() }.is_err()))
                .unwrap_or(false);
        }
        result
    }

    unsafe fn try_copy(&mut self, frame: *const ff::AVFrame, out: *mut ff::AVFrame) -> Result<bool, String> {
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
                // Only NVIDIA's driver was seen spinning a core in Map; an integrated Intel card sleeps there and lost frames at 4K with the fence
                let vendor = device.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).and_then(|a| a.GetDesc()).map(|d| d.VendorId).map_err(|e| e.to_string())?;
                if vendor != NVIDIA && !cfg!(test) {
                    return Ok(false);
                }
            }
            if self.fence.is_none() {
                let mut fence: Option<ID3D11Fence> = None;
                let made = device.cast::<ID3D11Device5>().and_then(|d| d.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence));
                let context4 = made.and_then(|_| context.cast::<ID3D11DeviceContext4>());
                self.no_fence = context4.is_err();
                let context4 = context4.map_err(|e| e.to_string())?;
                self.fence = Some((fence.ok_or("no fence")?, context4));
            }
            if self.event.is_none() {
                self.event = Some(CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?);
            }
            let (w, h) = ((*frame).width as u32, (*frame).height as u32);
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
                let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
                context.CopySubresourceRegion(&staging, 0, 0, 0, 0, source, index, Some(&region));
                context4.Signal(&fence, self.value).map_err(|e| e.to_string())?;
                context.Flush();
            }
            fence.SetEventOnCompletion(self.value, event).map_err(|e| e.to_string())?;
            // A missed signal must not stall the picture: Map below waits for the card anyway, and the fence is dropped after one miss
            if WaitForSingleObject(event, FENCE_WAIT_MS) != WAIT_OBJECT_0 {
                log::warn!("The card didn't signal the copy fence in {FENCE_WAIT_MS} ms; waiting in Map and using FFmpeg's copy from now on");
                self.failed = true;
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
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ffmpeg::{Codec, Hw};
    use crate::testenc;

    fn decoded(codec: &mut Codec, packets: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for packet in packets {
            codec.parse(packet, |codec, unit| {
                codec.decode_newest(unit).unwrap();
                if let Some(frame) = codec.take_newest().unwrap() {
                    frames.push(frame.data.to_vec());
                }
            });
        }
        frames
    }

    #[test]
    fn the_fenced_copy_gives_the_same_bytes_as_ffmpegs() {
        for (w, h) in [(256, 144), (1280, 720), (1920, 1080)] {
            let Some(packets) = testenc::h264_hd(6, w, h) else { return eprintln!("no H.264 encoder; skipped") };
            let Ok(mut fenced) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
            let Ok(mut plain) = Codec::h264(Some(Hw::D3d11va)) else { return eprintln!("no D3D11VA device; skipped") };
            plain.fenced_copy_off();
            let (a, b) = (decoded(&mut fenced, &packets), decoded(&mut plain, &packets));
            if fenced.fence_unavailable() {
                eprintln!("{w}x{h}: the driver has no fences; only the bytes are compared");
            } else {
                assert!(fenced.fenced_copy_works(), "{w}x{h}: the fenced copy failed and FFmpeg's took over");
            }
            assert_eq!(a.len(), b.len(), "{w}x{h}: picture count");
            assert!(!a.is_empty());
            for (n, (x, y)) in a.iter().zip(&b).enumerate() {
                assert!(x == y, "{w}x{h}: picture {n} differs from FFmpeg's copy");
            }
        }
    }
}
