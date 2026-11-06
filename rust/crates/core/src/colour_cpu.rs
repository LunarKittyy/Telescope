//! The colour LUT without the graphics card: what the Colour stage runs while core::balance says the card has no room. It has to pass for
//! the shader, so it goes the same way the card does: chroma spread out bilinearly, 8-bit RGB, the LUT, 8-bit RGB, and chroma back as the
//! average of each 2 x 2. Keeping to read_420 and rgb_to_420 would be cheaper, but their chroma is blockier and a swap would show.

use crate::colour::{Baked, LEVELS};
use crate::frame::{CpuStep, Frame, FrameContext, PixelFormat};
use crate::image::{byte_of, over_rows};
use crate::workers::each_part;
use std::sync::Arc;

const BANDS: usize = 8;

/// Where one 8-bit level lands along a side of the cube.
#[derive(Clone, Copy, Default)]
struct Spot {
    low: u32,
    /// How far towards the next point, 0..1 in 15 bits.
    weight: i16,
}

struct Cube {
    size: usize,
    /// [b][g][r] in 15 bits, which is what a vector's rounding multiply takes, with a spare to fill its lanes.
    points: Vec<[i16; 4]>,
    spots: [Spot; LEVELS],
}

enum Tables {
    Levels(Box<[[u8; LEVELS]; 3]>),
    Cube(Box<Cube>),
}

/// A baked LUT made ready for pixels, up front so a swap doesn't have to wait for anything.
pub struct Graded {
    tables: Arc<Tables>,
}

fn to_16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

impl Graded {
    pub fn new(baked: &Baked) -> Graded {
        let tables = match baked {
            Baked::Table(t) => {
                let mut levels = Box::new([[0u8; LEVELS]; 3]);
                for (i, px) in t.as_chunks::<3>().0.iter().enumerate().take(LEVELS) {
                    for c in 0..3 {
                        // Through 16 bits like the pictures the shader reads
                        levels[c][i] = ((to_16(px[c]) as u32 * 255 + 32767) / 65535) as u8;
                    }
                }
                Tables::Levels(levels)
            }
            // A cube whose size doesn't match its points can't be walked safely; it grades nothing
            Baked::Cube { size, rgb } if *size < 2 || size.checked_pow(3).and_then(|n| n.checked_mul(3)) != Some(rgb.len()) => {
                log::warn!("A colour cube of size {size} with {} values was ignored", rgb.len());
                let mut levels = Box::new([[0u8; LEVELS]; 3]);
                for level in levels.iter_mut() {
                    *level = std::array::from_fn(|i| i as u8);
                }
                Tables::Levels(levels)
            }
            Baked::Cube { size, rgb } => {
                let point = |v: f32| (to_16(v) >> 1) as i16;
                let points = rgb.as_chunks::<3>().0.iter().map(|p| [point(p[0]), point(p[1]), point(p[2]), 0]).collect();
                let mut spots = [Spot::default(); LEVELS];
                for (level, spot) in spots.iter_mut().enumerate() {
                    let at = level as f32 / 255.0 * (*size - 1) as f32;
                    let low = (at as usize).min(*size - 2);
                    *spot = Spot { low: low as u32, weight: ((at - low as f32) * 32767.0 + 0.5) as i16 };
                }
                Tables::Cube(Box::new(Cube { size: *size, points, spots }))
            }
        };
        Graded { tables: Arc::new(tables) }
    }

    fn grade(&self, r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
        match &*self.tables {
            Tables::Levels(levels) => {
                table_row(&levels[0], r);
                table_row(&levels[1], g);
                table_row(&levels[2], b);
            }
            Tables::Cube(cube) => cube_row(cube, r, g, b),
        }
    }

    /// source, Y then U then V planes, graded into out in the same layout.
    fn planar(&self, source: &[u8], w: usize, h: usize, out: &mut [u8]) {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (ys, chroma) = source.split_at(w * h);
        let (us, vs) = chroma.split_at(cw * ch);
        let (luma, rest) = out.split_at_mut(w * h);
        let (mut u_left, mut v_left) = rest.split_at_mut(cw * ch);
        let mut luma_left = luma;
        let per_band = ch.div_ceil(BANDS).max(1);
        let mut parts = Vec::with_capacity(BANDS);
        let mut first = 0;
        while first < ch {
            let cells = per_band.min(ch - first);
            let (y, rest) = luma_left.split_at_mut(w * (cells * 2).min(h - first * 2));
            luma_left = rest;
            let (u, rest) = u_left.split_at_mut(cells * cw);
            u_left = rest;
            let (v, rest) = v_left.split_at_mut(cells * cw);
            v_left = rest;
            parts.push((first, y, u, v));
            first += cells;
        }
        each_part(&mut parts, |_, (first, y, u, v)| {
            let mut wide = vec![0u16; 2 * w + cw];
            let mut bytes = vec![0u8; 6 * w];
            for (i, ((lines, u), v)) in y.chunks_mut(2 * w).zip(u.chunks_exact_mut(cw)).zip(v.chunks_exact_mut(cw)).enumerate() {
                let cell = *first + i;
                let rows = lines.len() / w;
                let (top, lower) = bytes.split_at_mut(3 * w);
                let (tr, rest) = top.split_at_mut(w);
                let (tg, tb) = rest.split_at_mut(w);
                let (br, rest) = lower.split_at_mut(w);
                let (bg, bb) = rest.split_at_mut(w);
                for row in 0..2 {
                    // The chroma row nearest this luma row and the one on its other side
                    let far = if row == 0 { cell.saturating_sub(1) } else { (cell + 1).min(ch - 1) };
                    let (spread_u, rest) = wide.split_at_mut(w);
                    let (spread_v, side) = rest.split_at_mut(w);
                    spread(&us[cell * cw..][..cw], &us[far * cw..][..cw], &mut side[..cw], spread_u);
                    spread(&vs[cell * cw..][..cw], &vs[far * cw..][..cw], &mut side[..cw], spread_v);
                    let (r, g, b) = if row == 0 { (&mut *tr, &mut *tg, &mut *tb) } else { (&mut *br, &mut *bg, &mut *bb) };
                    // An odd last row only has its own luma, so it counts for both
                    let at = (cell * 2 + row.min(rows - 1)) * w;
                    to_rgb(&ys[at..at + w], spread_u, spread_v, r, g, b);
                    self.grade(r, g, b);
                }
                let (first_line, second_line) = lines.split_at_mut(w);
                to_luma(tr, tg, tb, first_line);
                if rows == 2 {
                    to_luma(br, bg, bb, second_line);
                }
                to_chroma([&*tr, &*tg, &*tb], [&*br, &*bg, &*bb], u, v);
            }
        });
    }

    fn packed(&self, data: &mut [u8], w: usize, red: usize) {
        const RUN: usize = 256;
        over_rows(data, w * 4, |_, line| {
            let (mut r, mut g, mut b) = ([0u8; RUN], [0u8; RUN], [0u8; RUN]);
            for run in line.chunks_mut(RUN * 4) {
                let n = run.len() / 4;
                for (i, px) in run.as_chunks::<4>().0.iter().enumerate() {
                    (r[i], g[i], b[i]) = (px[red], px[1], px[2 - red]);
                }
                self.grade(&mut r[..n], &mut g[..n], &mut b[..n]);
                for (i, px) in run.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    px[red] = r[i];
                    px[1] = g[i];
                    px[2 - red] = b[i];
                    px[3] = 255;
                }
            }
        });
    }
}

crate::avx2_too! {
    /// One 8-bit plane of a row through its table.
    fn table_row(table: &[u8; LEVELS], plane: &mut [u8]) {
        for v in plane {
            *v = table[*v as usize];
        }
    }
}

/// From a point's 15 bits to 8, rounded.
#[inline(always)]
fn down(v: i32) -> u8 {
    ((v * 255 + (1 << 14)) >> 15) as u8
}

/// A row of pixels through the cube, interpolated in three steps as the shader mixes them.
fn cube_row(cube: &Cube, r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the processor was just asked whether it has AVX2
        return unsafe { cube_row_wide(cube, r, g, b) };
    }
    cube_row_plain(cube, r, g, b);
}

fn cube_row_plain(cube: &Cube, r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
    #[inline(always)]
    fn mix(a: [i16; 4], c: [i16; 4], weight: i16) -> [i16; 4] {
        std::array::from_fn(|i| a[i].wrapping_add(((c[i].wrapping_sub(a[i]) as i32 * weight as i32 + (1 << 14)) >> 15) as i16))
    }
    let n = cube.size;
    let (dg, db) = (n, n * n);
    for (r, (g, b)) in r.iter_mut().zip(g.iter_mut().zip(b.iter_mut())) {
        let (sr, sg, sb) = (cube.spots[*r as usize], cube.spots[*g as usize], cube.spots[*b as usize]);
        let at = sb.low as usize * db + sg.low as usize * dg + sr.low as usize;
        let c = &cube.points[at..at + db + dg + 2];
        let near = mix(mix(c[0], c[1], sr.weight), mix(c[dg], c[dg + 1], sr.weight), sg.weight);
        let high = mix(mix(c[db], c[db + 1], sr.weight), mix(c[db + dg], c[db + dg + 1], sr.weight), sg.weight);
        let out = mix(near, high, sb.weight);
        (*r, *g, *b) = (down(out[0] as i32), down(out[1] as i32), down(out[2] as i32));
    }
}

/// The same a third the price, which the compiler doesn't find by itself: four neighbours along red in one go, then a rounding multiply.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn cube_row_wide(cube: &Cube, r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
    use std::arch::x86_64::*;
    let n = cube.size;
    let (dg, db) = (n, n * n);
    let last = cube.points.len() - (db + dg + 2);
    for (r, (g, b)) in r.iter_mut().zip(g.iter_mut().zip(b.iter_mut())) {
        let (sr, sg, sb) = (cube.spots[*r as usize], cube.spots[*g as usize], cube.spots[*b as usize]);
        let at = (sb.low as usize * db + sg.low as usize * dg + sr.low as usize).min(last);
        // SAFETY: at is at most last, so each load's two points are inside points
        let pair = |i: usize| unsafe { _mm_loadu_si128(cube.points.as_ptr().add(i).cast()) };
        let (near, high) = (_mm256_set_m128i(pair(at + dg), pair(at)), _mm256_set_m128i(pair(at + db + dg), pair(at + db)));
        let weight = _mm256_set1_epi16(sr.weight);
        let along_red = |v: __m256i| {
            let (from, to) = (_mm256_unpacklo_epi64(v, v), _mm256_unpackhi_epi64(v, v));
            _mm256_add_epi16(from, _mm256_mulhrs_epi16(_mm256_sub_epi16(to, from), weight))
        };
        let (near, high) = (along_red(near), along_red(high));
        let lerp = |from: __m128i, to: __m128i, weight: i16| _mm_add_epi16(from, _mm_mulhrs_epi16(_mm_sub_epi16(to, from), _mm_set1_epi16(weight)));
        let near = lerp(_mm256_castsi256_si128(near), _mm256_extracti128_si256::<1>(near), sg.weight);
        let high = lerp(_mm256_castsi256_si128(high), _mm256_extracti128_si256::<1>(high), sg.weight);
        let out = _mm_cvtsi128_si64(lerp(near, high, sb.weight)) as u64;
        (*r, *g, *b) = (down((out & 0xffff) as i32), down((out >> 16 & 0xffff) as i32), down((out >> 32 & 0xffff) as i32));
    }
}

crate::avx2_too! {
    /// Two chroma rows as one value a luma pixel, sixteen times the size: 3:1 from the nearer row, 3:1 from the nearer sample, what a
    /// linear sampler gives at the middle of a luma pixel. `side` is room for the row between.
    fn spread(near: &[u8], far: &[u8], side: &mut [u16], out: &mut [u16]) {
        for (s, (n, f)) in side.iter_mut().zip(near.iter().zip(far)) {
            *s = 3 * *n as u16 + *f as u16;
        }
        let last = side.len() - 1;
        out[0] = 4 * side[0];
        for (i, o) in out[1..].as_chunks_mut::<2>().0.iter_mut().enumerate().take(last) {
            o[0] = 3 * side[i] + side[i + 1];
            o[1] = 3 * side[i + 1] + side[i];
        }
        if out.len().is_multiple_of(2) {
            out[last * 2 + 1] = 4 * side[last];
        }
    }
}

crate::avx2_too! {
    /// A row of luma with its spread chroma to R, G and B planes, with the constants of convert.wgsl.
    fn to_rgb(y: &[u8], u: &[u16], v: &[u16], r: &mut [u8], g: &mut [u8], b: &mut [u8]) {
        let n = y.len();
        let (y, u, v, r, g, b) = (&y[..n], &u[..n], &v[..n], &mut r[..n], &mut g[..n], &mut b[..n]);
        for i in 0..n {
            let c = (y[i] as f32 - 16.0) * 1.164_383;
            let (d, e) = (u[i] as f32 / 16.0 - 128.0, v[i] as f32 / 16.0 - 128.0);
            r[i] = byte_of(c + 1.596_027 * e);
            g[i] = byte_of(c - 0.391_762 * d - 0.812_968 * e);
            b[i] = byte_of(c + 2.017_232 * d);
        }
    }
}

crate::avx2_too! {
    fn to_luma(r: &[u8], g: &[u8], b: &[u8], out: &mut [u8]) {
        let n = out.len();
        let (r, g, b, out) = (&r[..n], &g[..n], &b[..n], &mut out[..n]);
        for i in 0..n {
            out[i] = byte_of(16.0 + 0.256_788 * r[i] as f32 + 0.504_129 * g[i] as f32 + 0.097_906 * b[i] as f32);
        }
    }
}

crate::avx2_too! {
    /// Each chroma sample from the average of its 2 x 2, an odd last column from its own pixels. second is first again on an odd last row.
    fn to_chroma(first: [&[u8]; 3], second: [&[u8]; 3], u: &mut [u8], v: &mut [u8]) {
        let w = first[0].len();
        let cells = w / 2;
        let cell = |p: usize, i: usize| {
            (first[p][i * 2] as u32 + first[p][i * 2 + 1] as u32 + second[p][i * 2] as u32 + second[p][i * 2 + 1] as u32) as f32 / 4.0
        };
        let (u_even, v_even) = (&mut u[..cells], &mut v[..cells]);
        for i in 0..cells {
            let (r, g, b) = (cell(0, i), cell(1, i), cell(2, i));
            u_even[i] = byte_of(128.0 - 0.148_223 * r - 0.290_993 * g + 0.439_216 * b);
            v_even[i] = byte_of(128.0 + 0.439_216 * r - 0.367_788 * g - 0.071_427 * b);
        }
        if w % 2 == 1 {
            let last = |p: usize| (first[p][w - 1] as u32 + second[p][w - 1] as u32) as f32 / 2.0;
            let (r, g, b) = (last(0), last(1), last(2));
            u[cells] = byte_of(128.0 - 0.148_223 * r - 0.290_993 * g + 0.439_216 * b);
            v[cells] = byte_of(128.0 + 0.439_216 * r - 0.367_788 * g - 0.071_427 * b);
        }
    }
}

impl CpuStep for Graded {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let source = frame.data.clone();
        match frame.format {
            PixelFormat::I420 => self.planar(&source, w, h, frame.data_mut()),
            PixelFormat::Nv12 => {
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                let (mut flat, mut out) = (Vec::with_capacity(source.len()), vec![0; source.len()]);
                flat.extend_from_slice(&source[..w * h]);
                let pairs = source[w * h..].as_chunks::<2>().0;
                flat.extend(pairs.iter().map(|p| p[0]));
                flat.extend(pairs.iter().map(|p| p[1]));
                self.planar(&flat, w, h, &mut out);
                let data = frame.data_mut();
                data[..w * h].copy_from_slice(&out[..w * h]);
                let (us, vs) = out[w * h..].split_at(cw * ch);
                for (pair, (u, v)) in data[w * h..].as_chunks_mut::<2>().0.iter_mut().zip(us.iter().zip(vs)) {
                    *pair = [*u, *v];
                }
            }
            PixelFormat::Rgba => self.packed(frame.data_mut(), w, 0),
            PixelFormat::Bgra => self.packed(frame.data_mut(), w, 2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour::{bake, lattice};
    use crate::host::ColourStage;

    fn cube(f: impl Fn(f32) -> f32, per_channel: bool) -> Baked {
        let mut rgb = lattice(33);
        // Red leaks into blue, so a grade that keeps channels apart wouldn't pass by accident
        for px in rgb.as_chunks_mut::<3>().0 {
            let mixed = (px[0] * 0.2 + px[2] * 0.8).clamp(0.0, 1.0);
            *px = [f(px[0]), f(px[1]), if per_channel { f(px[2]) } else { f(mixed) }];
        }
        let table = per_channel.then(|| Arc::new((0..LEVELS).flat_map(|i| [f(i as f32 / 255.0); 3]).collect::<Vec<f32>>()));
        bake(&[&ColourStage::new(33, Arc::new(rgb), table)], None).unwrap()
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut x = 7u32;
        (0..len).map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        }).collect()
    }

    struct Nothing;

    impl FrameContext for Nothing {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    #[test]
    fn pixels_come_out_as_the_lut_says() {
        for lut in [cube(|v| v * v, true), cube(|v| v.sqrt(), false)] {
            let graded = Graded::new(&lut);
            let px = noise(4096 * 3);
            let (mut r, mut g, mut b): (Vec<u8>, Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new(), Vec::new());
            for p in px.as_chunks::<3>().0 {
                r.push(p[0]);
                g.push(p[1]);
                b.push(p[2]);
            }
            graded.grade(&mut r, &mut g, &mut b);
            for (i, p) in px.as_chunks::<3>().0.iter().enumerate() {
                let want = lut.apply(*p);
                for (have, want) in [r[i], g[i], b[i]].into_iter().zip(want) {
                    assert!((have as i32 - want as i32).abs() <= 1, "{p:?}: {:?} and {want:?}", [r[i], g[i], b[i]]);
                }
            }
        }
    }

    #[test]
    fn a_cube_of_the_wrong_size_grades_nothing() {
        for (size, len) in [(33, 3 * 5), (1, 3), (0, 0), (usize::MAX, 3), (4, 3 * 64 + 3)] {
            let graded = Graded::new(&Baked::Cube { size, rgb: vec![0.5; len] });
            let px = noise(3000);
            let (mut r, mut g, mut b) = (px[..1000].to_vec(), px[1000..2000].to_vec(), px[2000..].to_vec());
            graded.grade(&mut r, &mut g, &mut b);
            assert!(r == px[..1000] && g == px[1000..2000] && b == px[2000..], "{size} {len}");
        }
    }

    #[test]
    fn the_vector_way_through_the_cube_gives_what_the_plain_way_does() {
        let Baked::Cube { .. } = cube(|v| v, false) else { panic!("a cube") };
        let graded = Graded::new(&cube(|v| v.sqrt(), false));
        let Tables::Cube(tables) = &*graded.tables else { panic!("a cube") };
        let px = noise(30_000);
        let (mut r, mut g, mut b) = (px[..10_000].to_vec(), px[10_000..20_000].to_vec(), px[20_000..].to_vec());
        let (mut r2, mut g2, mut b2) = (r.clone(), g.clone(), b.clone());
        cube_row(tables, &mut r, &mut g, &mut b);
        cube_row_plain(tables, &mut r2, &mut g2, &mut b2);
        for (a, b) in r.iter().chain(&g).chain(&b).zip(r2.iter().chain(&g2).chain(&b2)) {
            assert!((*a as i32 - *b as i32).abs() <= 1);
        }
    }

    #[test]
    fn a_grade_that_changes_nothing_keeps_a_smooth_picture_as_it_is() {
        let lut = cube(|v| v, true);
        let (w, h) = (64usize, 36usize);
        let mut data: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| (40 + x * 2 + y) as u8)).collect();
        data.extend((0..2 * (w / 2) * (h / 2)).map(|_| 128));
        let mut frame = Frame::new(w as u32, h as u32, PixelFormat::I420, data.clone());
        Graded::new(&lut).process(&mut frame, &Nothing);
        assert!(frame.data.iter().zip(&data).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 1));
    }

    #[test]
    fn every_format_and_an_odd_size_keep_their_shape() {
        let lut = cube(|v| 1.0 - v, false);
        for (w, h) in [(64u32, 36u32), (63, 35), (2, 2), (1, 1)] {
            for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Rgba, PixelFormat::Bgra] {
                let mut frame = Frame::new(w, h, format, noise(format.size(w, h)));
                let (size, before) = (frame.data.len(), frame.data.clone());
                Graded::new(&lut).process(&mut frame, &Nothing);
                assert_eq!(frame.data.len(), size);
                assert!(*frame.data != *before, "{format:?} {w}x{h}");
            }
        }
    }

    #[test]
    fn red_and_blue_swap_places_in_bgra() {
        let lut = cube(|v| v * 0.5, true);
        let mut rgba = Frame::new(1, 1, PixelFormat::Rgba, vec![200, 100, 50, 7]);
        let mut bgra = Frame::new(1, 1, PixelFormat::Bgra, vec![50, 100, 200, 7]);
        let mut graded = Graded::new(&lut);
        graded.process(&mut rgba, &Nothing);
        graded.process(&mut bgra, &Nothing);
        assert_eq!(rgba.data[..3], [100, 50, 25]);
        assert_eq!(bgra.data[..], [25, 50, 100, 255]);
    }
}
