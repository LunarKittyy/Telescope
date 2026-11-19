//! VA-API surfaces retained across decode, Vulkan import and a lazy RAM read.

use ash::vk;
use ffmpeg_sys_next as ff;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::frame::{DecodedSurface, Frame, PixelFormat};
use wgpu::hal::api::Vulkan;
use wgpu::hal::Device as _;

const MAX_SURFACES: usize = 6;
const R8: u32 = u32::from_le_bytes(*b"R8  ");
const GR88: u32 = u32::from_le_bytes(*b"GR88");
const INVALID_MODIFIER: u64 = (1u64 << 56) - 1;

#[derive(Default)]
pub(crate) struct Stream {
    disabled: AtomicBool,
    live: AtomicUsize,
    download: Mutex<Option<crate::ffmpeg::Codec>>,
    #[cfg(test)]
    forced: Mutex<Option<&'static str>>,
}

impl Stream {
    pub(crate) fn enabled(&self) -> bool {
        !self.disabled.load(Ordering::Acquire)
    }

    pub(crate) fn disable(&self, why: &str) {
        if !self.disabled.swap(true, Ordering::AcqRel) {
            log::warn!("VA-API surface import failed ({why}); copying decoded pictures through RAM for the rest of this stream");
        }
    }

    fn attempt<T>(&self, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if !self.enabled() {
            return Err("surface import is disabled for this stream".into());
        }
        #[cfg(test)]
        if let Some(why) = *self.forced.lock().unwrap() {
            self.disable(why);
            return Err(why.into());
        }
        work().inspect_err(|why| self.disable(why))
    }
}

struct AvFrame(*mut ff::AVFrame);

// SAFETY: each reference owns an AVFrame; access is serialised and FFmpeg's buffer references are thread safe.
unsafe impl Send for AvFrame {}

impl Drop for AvFrame {
    fn drop(&mut self) {
        // SAFETY: this frame was allocated or cloned by FFmpeg and is freed exactly once.
        unsafe { ff::av_frame_free(&mut self.0) }
    }
}

pub(crate) struct Surface {
    frame: Mutex<AvFrame>,
    ram: OnceLock<Frame>,
    stream: Arc<Stream>,
    node: Option<String>,
    size: (u32, u32),
    colour: u32,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaapiSurface").field("size", &self.size).field("landed", &self.ram.get().is_some()).finish()
    }
}

impl Surface {
    /// Retain the decoder's reference without mapping or downloading its pixels.
    pub(crate) unsafe fn capture(frame: *const ff::AVFrame, stream: Arc<Stream>, node: Option<String>) -> Result<Option<Arc<Self>>, String> {
        // SAFETY: the caller owns a live VA-API frame for this call; only metadata and a new FFmpeg reference are taken.
        let (size, colour, copy) = unsafe {
            if (*frame).width <= 0 || (*frame).height <= 0 || (*frame).hw_frames_ctx.is_null() {
                return Err("empty VA-API surface".into());
            }
            let ctx = &*((*(*frame).hw_frames_ctx).data as *const ff::AVHWFramesContext);
            if ctx.sw_format != ff::AVPixelFormat::AV_PIX_FMT_NV12 || (*frame).width % 2 != 0 || (*frame).height % 2 != 0 {
                return Err("surface import needs even-sized NV12".into());
            }
            let matrix = (*frame).colorspace as i32;
            let coefficients = std::slice::from_raw_parts(ff::sws_getCoefficients(matrix), 4);
            let bt601 = std::slice::from_raw_parts(ff::sws_getCoefficients(5), 4);
            let bt709 = std::slice::from_raw_parts(ff::sws_getCoefficients(1), 4);
            let colour = if coefficients == bt601 { 0 } else if coefficients == bt709 { 16 } else { return Err("unsupported surface colour matrix".into()) }
                | if (*frame).color_range == ff::AVColorRange::AVCOL_RANGE_JPEG { 32 } else { 0 };
            if stream.live.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_SURFACES).then_some(n + 1)).is_err() {
                return Ok(None);
            }
            (((*frame).width as u32, (*frame).height as u32), colour, ff::av_frame_clone(frame))
        };
        if copy.is_null() {
            stream.live.fetch_sub(1, Ordering::AcqRel);
            return Err("couldn't retain a VA-API surface".into());
        }
        Ok(Some(Arc::new(Self { frame: Mutex::new(AvFrame(copy)), ram: OnceLock::new(), stream, node, size, colour })))
    }

    pub(crate) fn disable(&self, why: &str) { self.stream.disable(why); }

    pub(crate) fn colour(&self) -> u32 {
        self.colour
    }

    pub(crate) fn import(&self, device: &wgpu::Device) -> Result<[wgpu::Texture; 2], String> {
        self.stream.attempt(|| {
            if device.adapter_info().backend != wgpu::Backend::Vulkan {
                return Err("the pipeline uses a non-Vulkan backend".into());
            }
            if !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
                return Err("Vulkan DMA-BUF extensions are unavailable on the pipeline device".into());
            }
            // SAFETY: the guard only borrows wgpu's Vulkan device and no wgpu calls are made until it is dropped.
            let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("no Vulkan device")?;
            same_device(&hal, self.node.as_deref())?;
            let source = self.frame.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the mapping is private and its source reference stays locked and alive until import has finished.
            let mapped = unsafe {
                let mapped = AvFrame(ff::av_frame_alloc());
                if mapped.0.is_null() {
                    return Err("couldn't allocate the DRM mapping".into());
                }
                (*mapped.0).format = ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
                // AV_HWFRAME_MAP_READ makes FFmpeg call vaSyncSurface before exporting separate DRM layers.
                let result = ff::av_hwframe_map(mapped.0, source.0, ff::AV_HWFRAME_MAP_READ as i32 | ff::AV_HWFRAME_MAP_DIRECT as i32);
                if result < 0 || (*mapped.0).data[0].is_null() {
                    return Err(format!("couldn't map the VA-API surface to DRM_PRIME ({result})"));
                }
                mapped
            };
            // SAFETY: FFmpeg owns an AVDRMFrameDescriptor at data[0] for this live DRM_PRIME mapping.
            let descriptor = unsafe { &*((*mapped.0).data[0] as *const ff::AVDRMFrameDescriptor) };
            let layouts = layouts(descriptor, self.size)?;
            let mut textures = Vec::with_capacity(2);
            for plane in layouts {
                let imported = (|| {
                    modifier_supported(&hal, &plane)?;
                    // SAFETY: descriptor validation checked the object index and fd; the live mapping owns this fd during duplication.
                    let fd: OwnedFd = unsafe { BorrowedFd::borrow_raw(plane.fd) }.try_clone_to_owned().map_err(|e| e.to_string())?;
                    let desc = wgpu::hal::TextureDescriptor {
                        label: Some("VA-API plane"),
                        size: wgpu::Extent3d { width: plane.width, height: plane.height, depth_or_array_layers: 1 },
                        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
                        format: plane.format, usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC,
                        memory_flags: wgpu::hal::MemoryFlags::empty(), view_formats: Vec::new(),
                    };
                    // SAFETY: the checked single-plane layout and modifier describe this duplicated DMA-BUF; hal consumes its fd.
                    let imported = unsafe { hal.texture_from_dmabuf_fd(fd, &desc, plane.modifier, plane.pitch, plane.offset) }.map_err(|e| e.to_string())?;
                    Ok((imported, desc.size, plane.format))
                })();
                match imported {
                    Ok(texture) => textures.push(texture),
                    Err(why) => {
                        for (texture, _, _) in textures {
                            // SAFETY: a previous plane was imported by this device but has not been wrapped or submitted.
                            unsafe { hal.destroy_texture(texture) };
                        }
                        return Err(why);
                    }
                }
            }
            drop(hal);
            let textures: Vec<_> = textures.into_iter().map(|(texture, size, format)| {
                let desc = wgpu::TextureDescriptor {
                    label: Some("VA-API plane"), size, mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
                };
                // SAFETY: the image belongs to this device and matches desc; external DMA-BUF contents use GENERAL, acquired before sampling.
                unsafe { device.create_texture_from_hal::<Vulkan>(texture, &desc, wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC) }
            }).collect();
            Ok(textures.try_into().unwrap_or_else(|_| unreachable!()))
        })
    }

    /// Acquire or release external queue ownership, keeping wgpu's tracked layouts in agreement with the raw barriers.
    pub(crate) fn ownership(device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, textures: &[wgpu::Texture; 2], acquire: bool) -> Result<(), String> {
        // SAFETY: all handles remain borrowed for the call; this encoder exclusively records the barriers outside any render pass.
        unsafe {
            let hal = device.as_hal::<Vulkan>().ok_or("no Vulkan device")?;
            let mut barriers = Vec::with_capacity(2);
            for texture in textures {
                let texture = texture.as_hal::<Vulkan>().ok_or("no Vulkan texture")?;
                barriers.push(vk::ImageMemoryBarrier::default()
                    .image(texture.raw_handle())
                    .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1))
                    .old_layout(if acquire { vk::ImageLayout::GENERAL } else { vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL })
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(if acquire { vk::QUEUE_FAMILY_EXTERNAL } else { hal.queue_family_index() })
                    .dst_queue_family_index(if acquire { hal.queue_family_index() } else { vk::QUEUE_FAMILY_EXTERNAL })
                    .src_access_mask(if acquire { vk::AccessFlags::empty() } else { vk::AccessFlags::SHADER_READ })
                    .dst_access_mask(if acquire { vk::AccessFlags::SHADER_READ } else { vk::AccessFlags::empty() }));
            }
            enc.as_hal_mut::<Vulkan, _, _>(|encoder| {
                let encoder = encoder.ok_or("no Vulkan command encoder")?;
                hal.raw_device().cmd_pipeline_barrier(encoder.raw_handle(),
                    if acquire { vk::PipelineStageFlags::TOP_OF_PIPE } else { vk::PipelineStageFlags::FRAGMENT_SHADER },
                    if acquire { vk::PipelineStageFlags::FRAGMENT_SHADER } else { vk::PipelineStageFlags::BOTTOM_OF_PIPE },
                    vk::DependencyFlags::empty(), &[], &[], &barriers);
                Ok(())
            })
        }
    }
}

impl DecodedSurface for Surface {
    fn ram(&self) -> &Frame {
        self.ram.get_or_init(|| {
            let source = self.frame.lock().unwrap_or_else(|e| e.into_inner());
            let mut download = self.stream.download.lock().unwrap_or_else(|e| e.into_inner());
            let out = (|| {
                if download.is_none() {
                    *download = Some(crate::ffmpeg::Codec::h264(None).map_err(|e| format!("{e:?}"))?);
                }
                download.as_mut().unwrap().download_surface(source.0).map_err(|e| format!("{e:?}"))
            })();
            out.unwrap_or_else(|why| {
                self.stream.disable(&why);
                let (w, h) = self.size;
                let mut data = vec![128; PixelFormat::I420.size(w, h)];
                data[..(w * h) as usize].fill(16);
                Frame::new(w, h, PixelFormat::I420, data)
            })
        })
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}

impl Drop for Surface {
    fn drop(&mut self) { self.stream.live.fetch_sub(1, Ordering::AcqRel); }
}

struct Plane {
    fd: i32,
    offset: u64,
    pitch: u64,
    modifier: u64,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

fn layouts(desc: &ff::AVDRMFrameDescriptor, (w, h): (u32, u32)) -> Result<[Plane; 2], String> {
    if desc.nb_layers != 2 || !(1..=4).contains(&desc.nb_objects) {
        return Err("DRM export is not two separate NV12 layers".into());
    }
    let mut result = Vec::new();
    for (i, (fourcc, format, width, height, bytes)) in [(R8, wgpu::TextureFormat::R8Unorm, w, h, 1u64), (GR88, wgpu::TextureFormat::Rg8Unorm, w / 2, h / 2, 2)].into_iter().enumerate() {
        let layer = &desc.layers[i];
        if layer.format != fourcc || layer.nb_planes != 1 {
            return Err("DRM layer has an unsupported format or auxiliary modifier planes".into());
        }
        let plane = &layer.planes[0];
        if !(0..desc.nb_objects).contains(&plane.object_index) || plane.offset < 0 || plane.pitch <= 0 {
            return Err("invalid DRM plane layout".into());
        }
        let object = &desc.objects[plane.object_index as usize];
        let (offset, pitch) = (plane.offset as u64, plane.pitch as u64);
        let end = pitch.checked_mul(height.saturating_sub(1) as u64).and_then(|v| v.checked_add(offset)).and_then(|v| v.checked_add(width as u64 * bytes));
        if object.fd < 0 || object.format_modifier == INVALID_MODIFIER || pitch < width as u64 * bytes || end.is_none_or(|v| v > object.size as u64) {
            return Err("invalid DMA-BUF size, fd or modifier".into());
        }
        result.push(Plane { fd: object.fd, offset, pitch, modifier: object.format_modifier, width, height, format });
    }
    Ok(result.try_into().unwrap_or_else(|_| unreachable!()))
}

fn same_device(hal: &wgpu::hal::vulkan::Device, node: Option<&str>) -> Result<(), String> {
    let node = node.ok_or("the decoder's render node is unknown")?;
    let node = std::fs::metadata(node).map_err(|e| e.to_string())?;
    let instance = hal.shared_instance().raw_instance();
    // SAFETY: the physical device belongs to this live instance; only its supported properties are queried.
    unsafe {
        let extensions = instance.enumerate_device_extension_properties(hal.raw_physical_device()).map_err(|e| e.to_string())?;
        if !extensions.iter().any(|e| std::ffi::CStr::from_ptr(e.extension_name.as_ptr()) == ash::ext::physical_device_drm::NAME) {
            return Err("Vulkan cannot identify the pipeline's DRM render node".into());
        }
        let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
        instance.get_physical_device_properties2(hal.raw_physical_device(), &mut properties);
        if drm.has_render == 0 || drm.render_major != libc::major(node.rdev()) as i64 || drm.render_minor != libc::minor(node.rdev()) as i64 {
            return Err("the decoder and pipeline use different cards".into());
        }
    }
    Ok(())
}

fn modifier_supported(hal: &wgpu::hal::vulkan::Device, plane: &Plane) -> Result<(), String> {
    let format = match plane.format { wgpu::TextureFormat::R8Unorm => vk::Format::R8_UNORM, _ => vk::Format::R8G8_UNORM };
    let instance = hal.shared_instance().raw_instance();
    // SAFETY: output arrays live across each query, the count is bounded, and the image description matches the hal importer.
    unsafe {
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut props = vk::FormatProperties2::default().push_next(&mut list);
        instance.get_physical_device_format_properties2(hal.raw_physical_device(), format, &mut props);
        if list.drm_format_modifier_count > 4096 {
            return Err("too many DRM modifiers".into());
        }
        let mut entries = vec![vk::DrmFormatModifierPropertiesEXT::default(); list.drm_format_modifier_count as usize];
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut entries);
        let mut props = vk::FormatProperties2::default().push_next(&mut list);
        instance.get_physical_device_format_properties2(hal.raw_physical_device(), format, &mut props);
        let needed = vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR | vk::FormatFeatureFlags::TRANSFER_SRC;
        if !entries.iter().any(|p| p.drm_format_modifier == plane.modifier && p.drm_format_modifier_plane_count == 1 && p.drm_format_modifier_tiling_features.contains(needed)) {
            return Err("Vulkan refused the DRM modifier for sampling".into());
        }
        let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut modifier = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default().drm_format_modifier(plane.modifier).sharing_mode(vk::SharingMode::EXCLUSIVE);
        let info = vk::PhysicalDeviceImageFormatInfo2::default().format(format).ty(vk::ImageType::TYPE_2D).tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC).push_next(&mut external).push_next(&mut modifier);
        let mut external = vk::ExternalImageFormatProperties::default();
        let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external);
        instance.get_physical_device_image_format_properties2(hal.raw_physical_device(), &info, &mut properties).map_err(|e| e.to_string())?;
        if !external.external_memory_properties.external_memory_features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE) {
            return Err("DRM modifier memory is not importable".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::{Codec, Hw};
    use crate::gpu::Gpu;
    use crate::testenc;

    #[test]
    fn import_errors_disable_only_that_stream_and_are_not_retried() {
        for reason in ["non-Vulkan backend", "missing extension", "device mismatch", "map failure", "modifier refused"] {
            let stream = Stream::default();
            *stream.forced.lock().unwrap() = Some(reason);
            assert!(stream.attempt(|| Ok(())).is_err());
            *stream.forced.lock().unwrap() = None;
            let mut tried = false;
            assert!(stream.attempt(|| { tried = true; Ok(()) }).is_err());
            assert!(!tried, "{reason} was retried");
            assert!(Stream::default().enabled(), "another stream was disabled too");
        }
    }

    fn descriptor() -> ff::AVDRMFrameDescriptor {
        // SAFETY: this plain C descriptor contains only integer fields and arrays; zero is valid for each field.
        let mut desc: ff::AVDRMFrameDescriptor = unsafe { std::mem::zeroed() };
        desc.nb_objects = 1;
        desc.objects[0].fd = 5;
        desc.objects[0].size = 32 * 16 * 3 / 2;
        desc.nb_layers = 2;
        for (i, format, offset) in [(0, R8, 0), (1, GR88, 32 * 16)] {
            desc.layers[i].format = format;
            desc.layers[i].nb_planes = 1;
            desc.layers[i].planes[0].offset = offset;
            desc.layers[i].planes[0].pitch = 32;
        }
        desc
    }

    #[test]
    fn separate_nv12_layers_keep_their_sizes_offsets_and_shared_object() {
        let desc = descriptor();
        let planes = layouts(&desc, (32, 16)).unwrap();
        assert_eq!((planes[0].width, planes[0].height, planes[0].offset), (32, 16, 0));
        assert_eq!((planes[1].width, planes[1].height, planes[1].offset), (16, 8, 512));
        assert_eq!(planes[0].fd, planes[1].fd);
    }

    #[test]
    fn unsupported_or_invalid_drm_descriptors_are_refused_before_vulkan() {
        for corrupt in [0, 1, 2, 3, 4, 5, 6, 7] {
            let mut desc = descriptor();
            match corrupt {
                0 => desc.nb_objects = 5,
                1 => desc.layers[0].nb_planes = 2,
                2 => desc.layers[1].planes[0].object_index = -1,
                3 => desc.layers[1].planes[0].offset = -1,
                4 => desc.layers[1].planes[0].pitch = 0,
                5 => desc.objects[0].format_modifier = INVALID_MODIFIER,
                6 => desc.objects[0].size = 1,
                _ => desc.layers[1].format = R8,
            }
            assert!(layouts(&desc, (32, 16)).is_err(), "invalid descriptor {corrupt} was accepted");
        }
    }

    fn decode_surface(codec: &mut Codec, packets: &[Vec<u8>]) -> Option<Frame> {
        let mut frame = None;
        for packet in packets {
            codec.parse(packet, |codec, unit| {
                codec.decode_newest(unit).unwrap();
                if let Some(newest) = codec.take_newest().unwrap() { frame = Some(newest); }
            });
        }
        frame
    }

    #[test]
    fn an_imported_vaapi_picture_matches_the_transfer_path() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no Vulkan device; skipped") };
        let Ok(mut codec) = Codec::h264(Some(Hw::Vaapi)) else { return eprintln!("no VA-API device; skipped") };
        codec.defer_surfaces = true;
        for (y, u, v, matrix, range) in [(70, 112, 145, "smpte170m", "tv"), (170, 150, 110, "bt709", "tv"), (110, 100, 152, "bt709", "pc")] {
            let Some(packets) = testenc::h264_yuv(y, u, v, 64, 48, matrix, range) else { return eprintln!("no H.264 encoder; skipped") };
            let frame = decode_surface(&mut codec, &packets).expect("a decoded frame");
            assert!(frame.data.is_empty(), "decode downloaded pixels before a RAM reader");
            let surface = frame.surface().unwrap().as_any().downcast_ref::<Surface>().unwrap();
            let mut imported = gpu.lift_with(&frame, true).unwrap();
            assert!(imported.was_imported(), "VA-API device exists but DMA-BUF import fell back: {}", !surface.stream.enabled());
            let imported_pixels = gpu.land(&mut imported).unwrap();
            gpu.put_down(imported);
            assert!(surface.ram.get().is_none(), "import used the transfer path");
            let transfer = frame.materialise();
            assert_eq!(frame.materialise().data, transfer.data, "the RAM landing is cached");
            let mut uploaded = gpu.lift_with(&transfer, true).unwrap();
            let transferred_pixels = gpu.land(&mut uploaded).unwrap();
            gpu.put_down(uploaded);
            let worst = imported_pixels.data.iter().zip(transferred_pixels.data.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= 3, "{matrix}/{range}: imported and transferred pixels differ by {worst} levels");
        }
    }

    #[test]
    fn forced_import_failures_still_transfer_this_and_following_vaapi_frames() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let Ok(mut codec) = Codec::h264(Some(Hw::Vaapi)) else { return eprintln!("no VA-API device; skipped") };
        codec.defer_surfaces = true;
        let Some(packets) = testenc::h264(&[70, 120, 180], 64, 48, 1) else { return eprintln!("no H.264 encoder; skipped") };
        let first = decode_surface(&mut codec, &packets[..1]).expect("a frame");
        let surface = first.surface().unwrap().as_any().downcast_ref::<Surface>().unwrap();
        *surface.stream.forced.lock().unwrap() = Some("forced modifier refusal");
        let mut held = gpu.lift_with(&first, true).unwrap();
        assert!(!held.was_imported());
        let out = gpu.land(&mut held).unwrap();
        gpu.put_down(held);
        assert!(out.data[0].abs_diff(70) <= 3, "fallback lost the original pixels");
        assert!(!surface.stream.enabled());
        let next = decode_surface(&mut codec, &packets[1..]).expect("another frame");
        assert!(next.surface().is_none(), "the decoder kept offering surfaces after fallback");
        assert!(next.data[0].abs_diff(180) <= 3);
    }
}
