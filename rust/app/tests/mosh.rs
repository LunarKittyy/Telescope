//! Datamosh through a real H.264 encoder and decoder (test_h264_mosh.py): the Mosher drops keyframes and renumbers the frames after
//! them, and what the decoder makes of that is checked on a stream with a moving square. Skipped where this build has no encoder.

use std::sync::{Arc, Mutex};
use telescope_core::host::{PacketStep, PacketStepFactory};
use telescope_core::media::{Codec, DecodeOptions, Decoder, Media, Packet};
use telescope_effects::mosh::{Mosh, Mosher, MosherFactory};
use telescope_media::FfmpegMedia;

const AUD: &[u8] = b"\x00\x00\x00\x01\x09\xf0";
const SIZE: (u32, u32) = (96, 64);

/// The packets of the test stream, or None here (and the test passes by doing nothing) without an encoder.
fn units(count: usize, gop: u32, profile: &str) -> Option<Vec<Vec<u8>>> {
    let units = telescope_media::test_h264_moving(count, SIZE.0, SIZE.1, gop, profile);
    if units.is_none() {
        eprintln!("this build has no H.264 encoder: skipped");
    }
    units
}

/// (start, end) of each NAL unit after its start code.
fn nals(data: &[u8]) -> Vec<(usize, usize)> {
    let starts: Vec<usize> = (0..data.len().saturating_sub(2)).filter(|&i| data[i..i + 3] == [0, 0, 1]).map(|i| i + 3).collect();
    starts.iter().enumerate().map(|(i, &s)| {
        let mut e = starts.get(i + 1).map_or(data.len(), |n| n - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        (s, e)
    }).collect()
}

fn is_keyframe(unit: &[u8]) -> bool {
    nals(unit).iter().any(|&(s, _)| unit[s] & 31 == 5)
}

/// The unit's SPS and PPS with start codes, and everything else.
fn split(unit: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut config, mut rest) = (Vec::new(), Vec::new());
    for (s, e) in nals(unit) {
        let target = if matches!(unit[s] & 31, 7 | 8) { &mut config } else { &mut rest };
        target.extend_from_slice(&[0, 0, 0, 1]);
        target.extend_from_slice(&unit[s..e]);
    }
    (config, rest)
}

fn decoder() -> Box<dyn Decoder> {
    FfmpegMedia.decoder(Codec::H264, &DecodeOptions { hardware: false }).expect("an H.264 decoder")
}

/// The grey pictures a decoder makes of the units, each one first run through `step`.
fn decode(units: &[Vec<u8>], mut step: impl FnMut(usize, &[u8]) -> Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut dec = decoder();
    let mut pictures = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        for u in step(i, unit) {
            let out = dec.decode(&Packet { data: [u, AUD.to_vec()].concat(), time_ns: 0 });
            if let Some(frame) = out.newest {
                pictures.push(frame.data[..(frame.width * frame.height) as usize].to_vec());
            }
        }
    }
    pictures
}

fn plain(_: usize, unit: &[u8]) -> Vec<Vec<u8>> {
    vec![unit.to_vec()]
}

fn gap(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs() as f64).sum::<f64>() / a.len() as f64
}

fn settings(mosh: Mosh) -> Arc<Mutex<Mosh>> {
    Arc::new(Mutex::new(mosh))
}

#[test]
fn dropped_keyframes_smear_the_old_picture_instead_of_freezing() {
    let Some(units) = units(30, 6, "high") else { return };
    let clean = decode(&units, plain);
    let mut mosher = Mosher::new(settings(Mosh::default()), false);
    let moshed = decode(&units, |_, u| mosher.step(u));
    let keyframes = units.iter().filter(|u| is_keyframe(u)).count();
    assert_eq!(moshed.len(), clean.len() - (keyframes - 1), "every frame but the dropped keyframes still decodes");
    let after = &moshed[6..];
    assert!(after.iter().zip(&clean[7..]).any(|(a, c)| gap(a, c) > 10.0));
    let distinct: std::collections::HashSet<&Vec<u8>> = after.iter().collect();
    assert!(distinct.len() > after.len() / 2, "it moves, not a frozen picture");
}

#[test]
fn clean_up_lets_the_next_keyframe_through_and_the_picture_comes_back() {
    let Some(units) = units(30, 6, "high") else { return };
    let clean = decode(&units, plain);
    let (headers, _) = split(&units[0]);
    let mut mosher = Mosher::new(settings(Mosh::default()), false);
    let moshed = decode(&units, |i, unit| {
        let mut unit = unit.to_vec();
        if i == 15 {
            // Clean up puts a new one on, which the decoder hands the stream's SPS and PPS first
            mosher = Mosher::new(settings(Mosh::default()), true);
            unit = [headers.clone(), unit].concat();
        }
        mosher.step(&unit)
    });
    assert!(gap(&moshed[14], &clean[16]) > 10.0, "keyframes 6 and 12 dropped");
    assert!(moshed[14..22].iter().any(|m| gap(m, &clean[20]) < 3.0), "18 went through");
    assert!(gap(moshed.last().unwrap(), clean.last().unwrap()) > 10.0, "and 24 is dropped again");
}

#[test]
fn put_on_mid_stream_it_moshes_from_the_next_keyframe() {
    let Some(units) = units(14, 6, "high") else { return };
    let (headers, _) = split(&units[0]);
    let mut mosher = Mosher::new(settings(Mosh::default()), false);
    let moshed = decode(&units, |i, unit| match i {
        // The decoder hands a new step the SPS and PPS first
        3 => mosher.step(&[headers.clone(), unit.to_vec()].concat()),
        i if i > 3 => mosher.step(unit),
        _ => vec![unit.to_vec()],
    });
    assert_eq!(moshed.len(), decode(&units, plain).len() - 2, "keyframes 6 and 12");
}

#[test]
fn repeats_hand_back_more_frames_and_all_of_them_decode() {
    let Some(units) = units(12, 100, "high") else { return };
    let mut mosher = Mosher::new(settings(Mosh { drop_keyframes: false, repeats: 2 }), false);
    assert_eq!(decode(&units, |_, u| mosher.step(u)).len(), 1 + 3 * 11);
}

#[test]
fn the_first_keyframe_always_goes_through() {
    let Some(units) = units(8, 6, "high") else { return };
    assert_eq!(Mosher::new(settings(Mosh::default()), false).step(&units[0]), vec![units[0].clone()]);
}

#[test]
fn a_baseline_stream_moshes_too() {
    let Some(units) = units(18, 6, "baseline") else { return };
    let mut mosher = Mosher::new(settings(Mosh::default()), false);
    assert_eq!(decode(&units, |_, u| mosher.step(u)).len(), 18 - 2);
}

#[test]
fn a_new_decoder_on_the_same_stream_gets_its_keyframes_after_a_reconnect() {
    let Some(units) = units(60, 10, "baseline") else { return };
    let config = split(&units[0]).0;
    let body: Vec<Vec<u8>> = units.iter().map(|u| split(u).1).collect();
    let tap = FfmpegMedia.packet_tap().expect("a packet tap");
    let factory: Arc<dyn PacketStepFactory> = Arc::new(MosherFactory { settings: settings(Mosh::default()), clean_up: false });
    tap.set(vec![("fun_pack".into(), factory)]);
    let run = |units: &[Vec<u8>]| -> u32 {
        let mut dec = FfmpegMedia.decoder_with_tap(Codec::H264, &DecodeOptions { hardware: false }, tap.clone()).expect("a decoder");
        units.iter().enumerate().map(|(i, u)| {
            let data = [if i == 0 { config.clone() } else { Vec::new() }, u.clone(), AUD.to_vec()].concat();
            dec.decode(&Packet { data, time_ns: 0 }).count
        }).sum()
    };
    assert!(run(&body[..25]) >= 20);
    assert!(run(&body[30..60]) >= 25, "the step from before had seen a frame and would drop this keyframe");
}
