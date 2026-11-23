//! Fitting a card picture into the virtual camera's size before it comes back (shaders/fit.wgsl), the same bytes as the camera's fit on the CPU.

use super::*;
use telescope_core::image::{fit_box, linear_taps, Tap};

const FIT: &str = include_str!("shaders/fit.wgsl");
/// The levels the bars around a fitted picture have: black in video range, no colour.
const BARS: [u32; 3] = [16, 128, 128];

/// The fit shader's pipeline, built the first time a picture is fitted.
pub(super) struct Kit {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    /// The taps for the last picture and camera sizes fitted, which stay the same frame after frame.
    taps: Mutex<Option<((u32, u32, u32, u32), wgpu::Texture)>>,
}

fn taps_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Uint, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

/// One plane's part of a fit: the box it's scaled into and the taps along each side.
struct PlaneFit {
    size: (u32, u32),
    area: (u32, u32, u32, u32),
    xs: Vec<Tap>,
    ys: Vec<Tap>,
}

/// The three planes of an I420 picture of `from` fitted into `to`, as the camera's fit_frame lays them out.
fn plane_fits(from: (u32, u32), to: (u32, u32)) -> [PlaneFit; 3] {
    let (x, y, nw, nh) = fit_box(from.0, from.1, to.0, to.1);
    let luma = PlaneFit { size: to, area: (x, y, nw, nh), xs: linear_taps(from.0 as usize, nw as usize, false), ys: linear_taps(from.1 as usize, nh as usize, true) };
    let chroma = || {
        let (cw, ch) = (chroma(nw), chroma(nh));
        PlaneFit { size: (chroma(to.0), chroma(to.1)), area: (x / 2, y / 2, cw, ch), xs: linear_taps(chroma(from.0) as usize, cw as usize, false), ys: linear_taps(chroma(from.1) as usize, ch as usize, true) }
    };
    [luma, chroma(), chroma()]
}

impl Gpu {
    fn fit_kit(&self) -> &Kit {
        self.fit_kit.get_or_init(|| {
            let layout = self.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fit"), entries: &[loaded_entry(0), taps_entry(1), uniform_entry(2)] });
            let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("fit"), source: wgpu::ShaderSource::Wgsl(FIT.into()) });
            let pipeline_layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fit"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
            let pipeline = self.render_pipeline("fit", &pipeline_layout, &module, ("vs_main", "fs_main"), wgpu::TextureFormat::R8Unorm);
            Kit { layout, pipeline, taps: Mutex::new(None) }
        })
    }

    /// Every plane's taps on one texture, x then y for each, a row each, as (first, second, p, q) in 16 bits; made again only when a size changes.
    fn fit_taps(&self, kit: &Kit, from: (u32, u32), to: (u32, u32), fits: &[PlaneFit; 3]) -> Result<wgpu::Texture, String> {
        let key = (from.0, from.1, to.0, to.1);
        let mut kept = kit.taps.lock().unwrap();
        if let Some((_, texture)) = kept.as_ref().filter(|(k, _)| *k == key) {
            return Ok(texture.clone());
        }
        // Not trusted until the write is through
        *kept = None;
        let rows: Vec<&Vec<Tap>> = fits.iter().flat_map(|f| [&f.xs, &f.ys]).collect();
        let width = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1) as u32;
        if width > self.device.limits().max_texture_dimension_2d {
            return Err(format!("a camera {width} pixels across is wider than the card's textures"));
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fit taps"),
            size: wgpu::Extent3d { width, height: rows.len() as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut bytes = vec![0u8; width as usize * rows.len() * 8];
        for (y, row) in rows.iter().enumerate() {
            for (x, t) in row.iter().enumerate() {
                let at = (y * width as usize + x) * 8;
                for (i, v) in [t.0 as u16, t.1 as u16, t.2 as u16, t.3 as u16].into_iter().enumerate() {
                    bytes[at + i * 2..at + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        self.write_plane(&texture, &bytes, width * 8);
        *kept = Some((key, texture.clone()));
        Ok(texture)
    }

    /// The held I420 picture fitted into width x height as the virtual camera fits frames (image::fit_box, bars in black), scaled on the
    /// card so only the camera's size is read back. The card keeps its own copy as it was.
    pub fn land_fitted(&self, held: &mut Held, width: u32, height: u32, cancel: &dyn Fn() -> bool) -> Result<Frame, String> {
        #[cfg(test)]
        let _timing = transfers::measurement::time(transfers::measurement::Stage::Land);
        self.card_ok()?;
        if held.format != PixelFormat::I420 || width == 0 || height == 0 || held.width == 0 || held.height == 0 {
            return Err(format!("can't fit a {:?} picture into {width}x{height} on the card", held.format));
        }
        let (w, h) = (held.width, held.height);
        let fits = plane_fits((w, h), (width, height));
        // The picture's planes as they are now: the ones it holds, or what landing would make of its colours
        let rgba = if held.planes.is_none() { Some(self.ensure_rgba(held)) } else { None };
        let mut made = Vec::new();
        let (enc, targets, taps) = self.scoped(|| -> Result<_, String> {
            let kit = self.fit_kit();
            let taps = self.fit_taps(kit, (w, h), (width, height), &fits)?;
            let mut enc = std::mem::replace(&mut held.enc, self.device.create_command_encoder(&Default::default()));
            let planes = match (&held.planes, &rgba) {
                (Some(planes), _) => planes.clone(),
                (None, rgba) => {
                    let rgba = rgba.as_ref().expect("a picture without planes was given its colours above");
                    let work = [rgba.create_view(&Default::default()), self.blank.clone(), self.blank.clone()];
                    let planes = [self.texture(w, h, wgpu::TextureFormat::R8Unorm), self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm),
                                  self.texture(chroma(w), chroma(h), wgpu::TextureFormat::R8Unorm)];
                    for (plane, mode) in planes.iter().zip([Mode::ToY, Mode::ToU, Mode::ToV]) {
                        self.convert_pass(&mut enc, &work, mode, plane, 1, (w, h));
                    }
                    made.extend(planes.iter().cloned());
                    planes
                }
            };
            let taps_view = taps.create_view(&Default::default());
            let mut targets = Vec::new();
            for (i, (fit, plane)) in fits.iter().zip(&planes).enumerate() {
                let target = self.texture(fit.size.0, fit.size.1, wgpu::TextureFormat::R8Unorm);
                let (x, y, bw, bh) = fit.area;
                let settings = self.uniform(bytemuck_u32(&[x, y, bw, bh, BARS[i], 2 * i as u32, 2 * i as u32 + 1, 0]));
                let source = plane.create_view(&Default::default());
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("fit"),
                    layout: &kit.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&source) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&taps_view) },
                        wgpu::BindGroupEntry { binding: 2, resource: settings.as_entire_binding() },
                    ],
                });
                draw(&mut enc, &kit.pipeline, &[&bind], &target.create_view(&Default::default()));
                targets.push(target);
            }
            Ok((enc, targets, taps))
        }).inspect_err(|_| *self.fit_kit().taps.lock().unwrap() = None)??;
        let read = self.download_until(enc, &[&targets[0], &targets[1], &targets[2]], &[1, 1, 1], cancel);
        drop(taps);
        self.give_back(targets.into_iter().chain(made));
        let (out, waited) = read?;
        held.waited_ns += waited;
        let mut fitted = Frame::new(width, height, PixelFormat::I420, out);
        fitted.number = held.number;
        fitted.time_ns = held.time_ns;
        fitted.capture_ns = held.capture_ns;
        Ok(fitted)
    }
}

/// The camera's fit on the CPU, as shell's fit_frame does it (which the media crate can't reach): the same box, bars and taps.
#[cfg(test)]
pub(crate) fn cpu_fit(frame: &Frame, width: u32, height: u32) -> Vec<u8> {
    let (x, y, nw, nh) = fit_box(frame.width, frame.height, width, height);
    let mut out = vec![0u8; PixelFormat::I420.size(width, height)];
    let (sy, su, sv) = frame.i420_planes().unwrap();
    let (cw, ch) = (chroma(width) as usize, chroma(height) as usize);
    let (oy, rest) = out.split_at_mut((width * height) as usize);
    let (ou, ov) = rest.split_at_mut(cw * ch);
    oy.fill(16);
    ou.fill(128);
    ov.fill(128);
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    telescope_core::image::resize_linear_into(sy, fw, fh, 1, oy, width as usize, (x as usize, y as usize, nw as usize, nh as usize));
    let (fcw, fch) = (fw.div_ceil(2), fh.div_ceil(2));
    let in_chroma = ((x / 2) as usize, (y / 2) as usize, chroma(nw) as usize, chroma(nh) as usize);
    telescope_core::image::resize_linear_into(su, fcw, fch, 1, ou, cw, in_chroma);
    telescope_core::image::resize_linear_into(sv, fcw, fch, 1, ov, cw, in_chroma);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(w: u32, h: u32, seed: u32) -> Frame {
        let size = PixelFormat::I420.size(w, h);
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        let data = (0..size).map(|i| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            // A slope under the noise, so the blend has something to get wrong between neighbours
            ((i % 251) as u32 / 2 + (state & 0x7f)) as u8
        }).collect();
        Frame::new(w, h, PixelFormat::I420, data)
    }

    fn worst(a: &[u8], b: &[u8]) -> u8 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
    }

    #[test]
    fn a_picture_fitted_on_the_card_is_the_cpus_fit_to_the_byte() {
        let Some(gpu) = Gpu::shared() else { return };
        // Down from 4K and from odd sizes, letterboxed and pillarboxed, up from a small one, and a shape that needs no scaling but bars
        for (from, to) in [((3840, 2160), (1920, 1080)), ((1920, 1080), (1280, 720)), ((1921, 1079), (1280, 720)), ((1080, 1920), (1920, 1080)),
                           ((1920, 800), (1920, 1080)), ((2560, 1440), (1280, 960)), ((640, 360), (1920, 1080)), ((1279, 723), (853, 481))] {
            let frame = noisy(from.0, from.1, from.0 ^ to.1);
            let mut held = gpu.lift(&frame).unwrap();
            let fitted = gpu.land_fitted(&mut held, to.0, to.1, &|| false).unwrap();
            let expected = cpu_fit(&frame, to.0, to.1);
            assert_eq!((fitted.width, fitted.height, fitted.data.len()), (to.0, to.1, expected.len()), "{from:?} into {to:?}");
            assert_eq!(worst(&fitted.data, &expected), 0, "{from:?} into {to:?}");
            gpu.put_down(held);
        }
    }

    #[test]
    fn a_shaded_picture_is_fitted_from_what_landing_it_would_give() {
        let Some(gpu) = Gpu::shared() else { return };
        let frame = noisy(1920, 1080, 7);
        let invert = ShaderSlot::new(ShaderStep { keeps_geometry: false, wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4<f32>(1.0 - c.rgb, 1.0); }".into(), uniforms: serde_json::json!({}), textures: vec![] });
        let mut slot = invert;
        slot.prepare(&gpu).unwrap();
        let mut held = gpu.lift(&frame).unwrap();
        gpu.shade(&mut held, &slot, 0.0, None).unwrap();
        let fitted = gpu.land_fitted(&mut held, 1280, 720, &|| false).unwrap();
        // Landing after the fit still gives the whole picture, which the CPU's fit agrees with
        let landed = gpu.land(&mut held).unwrap();
        assert_eq!((landed.width, landed.height), (1920, 1080));
        assert_eq!(worst(&fitted.data, &cpu_fit(&landed, 1280, 720)), 0);
        gpu.put_down(held);
    }

    #[test]
    fn only_i420_pictures_are_fitted_on_the_card() {
        let Some(gpu) = Gpu::shared() else { return };
        let rgba = Frame::new(64, 32, PixelFormat::Rgba, vec![90; 64 * 32 * 4]);
        let mut held = gpu.lift(&rgba).unwrap();
        assert!(gpu.land_fitted(&mut held, 32, 16, &|| false).is_err());
        gpu.put_down(held);
    }
}
