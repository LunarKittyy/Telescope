//! The shader conversion against the CPU's, on WARP and on this machine's card if it has one.

use super::testing::{devices, extremes_then_noise, noise, texture_with, warp};
use super::*;
use crate::convert::{bgra_to_i420, rgb_to_i420};
use std::sync::atomic::Ordering;

const BGRA: u32 = 87;
const BGRX: u32 = 88;
const RGBA: u32 = 28;
const BGRA_SRGB: u32 = 91;
const RGBA_TYPELESS: u32 = 27;

/// What the CPU makes of bytes laid out as `format`.
fn cpu_planes(data: &[u8], (w, h): (u32, u32), format: u32) -> Vec<u8> {
    let frame = match format {
        RGBA | RGBA_TYPELESS => rgb_to_i420(data, w, h, 4, false),
        _ => bgra_to_i420(data, w, h),
    };
    frame.data.to_vec()
}

/// The largest difference between two planes' bytes, and how many differ at all.
fn compare(card: &[u8], cpu: &[u8]) -> (u8, usize) {
    assert_eq!(card.len(), cpu.len(), "lengths");
    card.iter().zip(cpu).map(|(a, b)| (a.abs_diff(*b), usize::from(a != b))).fold((0, 0), |(m, n), (d, o)| (m.max(d), n + o))
}

fn convert_on(gpu: &Gpu, texture: &SharedTexture) -> Vec<u8> {
    let converter = gpu.converter(texture).expect("a converter");
    gpu.copy_to_converter(&converter, texture).expect("a copy");
    gpu.convert(&converter).expect("planes")
}

#[test]
fn the_shaders_make_the_planes_the_cpu_makes_within_one_level() {
    let sizes = [(1, 1), (2, 2), (3, 3), (5, 3), (2, 7), (17, 9), (33, 31), (64, 48), (640, 360), (1279, 719), (1920, 1080)];
    for gpu in devices() {
        for size in sizes {
            for (n, format) in [BGRA, BGRX, RGBA, BGRA_SRGB, RGBA_TYPELESS].into_iter().enumerate() {
                let data = extremes_then_noise(size.0, size.1, n as u32 + size.0);
                let texture = texture_with(&gpu, size, format, &data);
                let card = convert_on(&gpu, &texture);
                let (most, differing) = compare(&card, &cpu_planes(&data, size, format));
                println!("{} {size:?} format {format}: largest difference {most}, {differing} of {} bytes differ", gpu.adapter, card.len());
                assert!(most <= 1, "{} {size:?} format {format}: off by {most}", gpu.adapter);
            }
        }
    }
}

#[test]
fn flat_colours_come_out_as_the_cpu_makes_them() {
    let gpu = warp();
    for pixel in [[0u8, 0, 0, 255], [255, 255, 255, 255], [255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [16, 16, 16, 255], [128, 128, 128, 0]] {
        for size in [(2, 2), (3, 3)] {
            let data: Vec<u8> = pixel.iter().copied().cycle().take(size.0 as usize * size.1 as usize * 4).collect();
            let card = convert_on(&gpu, &texture_with(&gpu, size, BGRA, &data));
            let (most, _) = compare(&card, &cpu_planes(&data, size, BGRA));
            assert!(most <= 1, "{pixel:?} {size:?}: off by {most}");
        }
    }
}

#[test]
fn the_next_picture_is_made_afresh_in_the_same_converter() {
    let gpu = warp();
    let size = (33, 17);
    let (a, b) = (noise(33 * 17 * 4, 1), noise(33 * 17 * 4, 2));
    let (first, second) = (texture_with(&gpu, size, BGRA, &a), texture_with(&gpu, size, BGRA, &b));
    let converter = gpu.converter(&first).unwrap();
    for (texture, data) in [(&first, &a), (&second, &b), (&first, &a)] {
        gpu.copy_to_converter(&converter, texture).unwrap();
        let card = gpu.convert(&converter).unwrap();
        assert!(compare(&card, &cpu_planes(data, size, BGRA)).0 <= 1);
    }
}

#[test]
fn a_failed_conversion_leaves_the_converter_and_its_copy_usable() {
    let gpu = warp();
    let size = (17, 9);
    let data = noise(17 * 9 * 4, 5);
    let texture = texture_with(&gpu, size, BGRA, &data);
    let converter = gpu.converter(&texture).unwrap();
    gpu.copy_to_converter(&converter, &texture).unwrap();
    converter.fail_next.store(true, Ordering::SeqCst);
    assert!(gpu.convert(&converter).is_err());
    // The copy is still there to read as BGRA, and the shaders still run
    let staging = gpu.staging(&texture, 4).unwrap();
    gpu.copy_source(&staging, &converter);
    assert_eq!(gpu.read(&staging).unwrap(), data);
    assert!(compare(&gpu.convert(&converter).unwrap(), &cpu_planes(&data, size, BGRA)).0 <= 1);
}

#[test]
fn a_texture_of_another_size_or_a_format_the_card_doesnt_take_is_refused() {
    let gpu = warp();
    let data = noise(4 * 4 * 4, 3);
    let texture = texture_with(&gpu, (4, 4), BGRA, &data);
    let converter = gpu.converter(&texture).unwrap();
    let other = texture_with(&gpu, (4, 2), BGRA, &data[..32]);
    assert!(gpu.copy_to_converter(&converter, &other).is_err());
    let mut odd = texture_with(&gpu, (4, 4), BGRA, &data);
    odd.format = 10;
    assert!(gpu.converter(&odd).is_err());
}

#[test]
fn planes_come_back_whatever_the_row_pitch_of_the_card_is() {
    // A width whose rows are no multiple of the card's pitch alignment, in every plane
    let gpu = warp();
    for width in [1u32, 3, 61, 63, 65, 127, 129, 255, 257] {
        let size = (width, 5);
        let data = noise(width as usize * 5 * 4, width);
        let card = convert_on(&gpu, &texture_with(&gpu, size, BGRA, &data));
        assert_eq!(card.len(), crate::spout_shared::planes::i420_len(width, 5));
        assert!(compare(&card, &cpu_planes(&data, size, BGRA)).0 <= 1, "width {width}");
    }
}
