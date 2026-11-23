//! The small picture the card makes for the vision models (Gpu::land_small).

use super::*;

/// A flat grey picture whose U climbs `du` a sample from left to right and V `dv` a sample from top to bottom.
fn ramps(w: u32, h: u32, du: usize, dv: usize) -> Frame {
    let (cw, ch) = (chroma(w) as usize, chroma(h) as usize);
    let mut data = vec![100u8; (w * h) as usize];
    data.extend((0..cw * ch).map(|i| ((i % cw) * du) as u8));
    data.extend((0..cw * ch).map(|i| ((i / cw) * dv) as u8));
    Frame::new(w, h, PixelFormat::I420, data)
}

/// How far each chroma sample of the small picture is from the colour at the same place in the big one.
fn worst_chroma_error(big: (u32, u32), small: (u32, u32), (du, dv): (usize, usize)) -> f64 {
    let Ok(gpu) = Gpu::open() else { return 0.0 };
    let mut held = gpu.lift(&ramps(big.0, big.1, du, dv)).unwrap();
    let out = gpu.land_small(&mut held, small, &|| false).unwrap().unwrap();
    gpu.put_down(held);
    let (cw, ch) = (chroma(small.0) as usize, chroma(small.1) as usize);
    let luma = (small.0 * small.1) as usize;
    assert!(out.data[..luma].iter().all(|&y| y.abs_diff(100) <= 1), "the luma of a flat picture stays flat");
    let (u, v) = (&out.data[luma..luma + cw * ch], &out.data[luma + cw * ch..]);
    let mut worst = 0.0f64;
    for j in 0..ch {
        for i in 0..cw {
            // The last sample of an odd-sized small picture is half over its edge, where a box average has only the picture to take from
            if (small.0 % 2 == 1 && i == cw - 1) || (small.1 % 2 == 1 && j == ch - 1) {
                continue;
            }
            // A chroma sample stands for a 2 x 2 block of the picture, so the sample i of the small one sits at (i + 0.5) * big / small of the big one's samples
            let (x, y) = ((i as f64 + 0.5) * big.0 as f64 / small.0 as f64, (j as f64 + 0.5) * big.1 as f64 / small.1 as f64);
            let want = |t: f64, step: usize, last: u32| (t - 0.5).clamp(0.0, last as f64 - 1.0) * step as f64;
            worst = worst.max((u[j * cw + i] as f64 - want(x, du, chroma(big.0))).abs()).max((v[j * cw + i] as f64 - want(y, dv, chroma(big.1))).abs());
        }
    }
    worst
}

#[test]
fn a_small_picture_of_odd_sizes_has_its_chroma_where_the_big_one_has_it() {
    // A picture an odd number of pixels wide has half a sample more chroma than it has luma to cover, which the shrink must not stretch across the picture
    for (big, small) in [((101, 61), (50, 30)), ((101, 61), (51, 31)), ((100, 60), (51, 31)), ((100, 60), (50, 30))] {
        let worst = worst_chroma_error(big, small, (5, 8));
        assert!(worst <= 1.0, "{big:?} shrunk to {small:?}: chroma off by {worst} levels");
    }
}

#[test]
fn a_refused_small_readback_leaves_nothing_to_land_from_the_card() {
    let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
    let mut held = gpu.lift(&ramps(64, 36, 4, 4)).unwrap();
    gpu.inject(Fault::RefusedSubmit);
    assert!(gpu.land_small(&mut held, (32, 18), &|| false).is_err());
    // The steps the refused submission carried never ran, so what the card holds isn't the picture and no readback may pass it off as one
    assert!(gpu.land(&mut held).is_err(), "landed a picture that was never made");
    assert!(gpu.land_small(&mut held, (32, 18), &|| false).is_err());
    gpu.put_down(held);
}
