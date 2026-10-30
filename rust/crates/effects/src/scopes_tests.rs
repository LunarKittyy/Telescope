//! tests/test_scopes.py, and the cv2 and numpy arithmetic it stands on.

use super::*;

/// A frame of one colour given as Python writes it, B, G, R.
fn frame(bgr: [u8; 3], w: usize, h: usize) -> Rgb {
    Rgb::filled(w, h, [bgr[2], bgr[1], bgr[0]])
}

fn noise(w: usize, h: usize) -> Rgb {
    let mut seed = 1u64;
    let data = (0..w * h * 3).map(|_| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 56) as u8
    }).collect();
    Rgb { w, h, data }
}

#[test]
fn shrink_keeps_the_shape_and_never_grows() {
    let big = Rgb::new(1920, 1080);
    let small = shrink(&big, SCOPE_WIDTH);
    assert_eq!((small.w, small.h), (256, 144));
    let tiny = Rgb::new(64, 36);
    assert_eq!(shrink(&tiny, SCOPE_WIDTH), tiny);
    assert_eq!(shrunk_size(64, 36, SCOPE_WIDTH), None);
}

#[test]
fn histogram_counts_every_pixel_at_its_level() {
    let f = frame([10, 20, 30], 64, 36);
    let counts = histogram(&f);
    let all = (f.w * f.h) as u32;
    assert_eq!((counts[0][10], counts[1][20], counts[2][30]), (all, all, all));
    for row in &counts {
        assert_eq!(row.iter().sum::<u32>(), all);
    }
    let level = (0.0722f64 * 10.0 + 0.7152 * 20.0 + 0.2126 * 30.0).round_ties_even() as usize;
    assert_eq!(counts[3][level], all);
}

#[test]
fn waveform_puts_bright_columns_at_the_top() {
    let mut channel = vec![0u8; 36 * 64];
    for y in 0..36 {
        for x in 32..64 {
            channel[y * 64 + x] = 255;
        }
    }
    let counts = waveform(&channel, 64, 36);
    assert_eq!(counts.len(), 256 * 64);
    // level 0 at the bottom, 255 at the top
    assert!((0..32).all(|x| counts[255 * 64 + x] == 36));
    assert!((32..64).all(|x| counts[x] == 36));
    assert_eq!(counts.iter().sum::<u32>() as usize, channel.len());
}

#[test]
fn parade_has_red_green_and_blue_in_that_order() {
    let counts = parade(&frame([0, 0, 255], 10, 36));
    let at = |row: usize, col: usize| counts[row * 30 + col];
    // pure red: red at the top, green and blue at the bottom
    assert!((0..10).all(|x| at(0, x) == 36));
    assert!((10..20).all(|x| at(255, x) == 36));
    assert!((20..30).all(|x| at(255, x) == 36));
}

#[test]
fn grey_sits_in_the_middle_of_the_vectorscope_and_red_up_and_left_of_it() {
    let grey = vectorscope(&frame([128, 128, 128], 64, 36), 256);
    assert_eq!(grey[128 * 256 + 128], 36 * 64);
    let red = vectorscope(&frame([0, 0, 255], 64, 36), 256);
    let top = (0..red.len()).max_by_key(|&i| (red[i], std::cmp::Reverse(i))).unwrap();
    let (x, y) = (top % 256, top / 256);
    assert_eq!((x, y), scope_point([255, 0, 0], 256));
    // little blue, a lot of red
    assert!(x < 128 && y < 128);
}

#[test]
fn every_scope_makes_a_bgra_picture() {
    let f = noise(960, 540);
    for name in SCOPE_NAMES {
        let image = scope_image(name, &f).unwrap();
        assert_eq!(image.data.len(), image.w * image.h * 4, "{name}");
        // the backdrop shows everywhere, so the scope reads over any picture
        assert!(image.data.as_chunks::<4>().0.iter().all(|p| p[3] > 0), "{name}");
    }
    assert!(scope_image("oscilloscope", &f).is_none());
}

#[test]
fn a_brighter_spot_glows_more() {
    let g = glow(&[0, 1, 10, 1000], 10.0);
    assert_eq!(g[0], 0.0);
    assert!(g[1] < g[2] && g[2] < g[3] && g[3] <= 1.0);
}

#[test]
fn the_scope_pictures_have_the_shapes_python_makes() {
    let f = noise(960, 540);
    let sizes: Vec<(usize, usize)> = SCOPE_NAMES.iter().map(|n| scope_image(n, &f).map(|i| (i.w, i.h)).unwrap()).collect();
    // the frame is shrunk to 256 x 144 first
    assert_eq!(sizes, [(256, 128), (256, 256), (3 * 256 + 8, 256), (256, 256)]);
}

#[test]
fn the_colour_arithmetic_is_opencvs() {
    // pure red lands on 255 in Cr, which OpenCV holds there
    assert_eq!(ycrcb_of(255, 0, 0), (76, 255, 85));
    assert_eq!(ycrcb_of(0, 0, 0), (0, 128, 128));
    assert_eq!(ycrcb_of(255, 255, 255), (255, 128, 128));
    assert_eq!(bgr_of_ycrcb(170, 128, 128), [170, 170, 170]);
    assert_eq!(luma_of(255, 255, 255), 255);
    assert_eq!(luma_of(0, 0, 0), 0);
}
