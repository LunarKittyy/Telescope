//! A measurement (not run with the tests): what decoding costs beside a heavy graphics load when the card copies each picture out as
//! I420 or as its own NV12, against software. Packets arrive at a steady 30 fps, as from a phone, and the load is tools/stress/loads.
//! cargo test --release -p telescope-media --lib -- --ignored --nocapture card_bench::measure
//! Env: BENCH_CLIP (p1080.h264), BENCH_MODES (sw,i420,nv12), BENCH_SECONDS (20), BENCH_LABEL (printed in each row)

use crate::ffmpeg::decode_bench::packets;
use crate::ffmpeg::{Codec, Hw};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_micros(33_333);

struct Record {
    sent: Instant,
    arrived: Instant,
    decode_ns: u64,
    copy_ns: u64,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Feeds the packets at 30 fps to one decoder on its own thread and returns when each picture came out.
fn run(mode: &str, all: &[Vec<u8>], seconds: u64) -> Vec<Record> {
    let count = (seconds * 30) as usize;
    let (tx, rx) = mpsc::channel::<(Instant, usize)>();
    let (done_tx, done_rx) = mpsc::channel::<Record>();
    let all: std::sync::Arc<Vec<Vec<u8>>> = std::sync::Arc::new(all.to_vec());
    let (hw, i420) = (mode != "sw", mode == "i420");
    let worker = {
        let all = all.clone();
        std::thread::spawn(move || {
            let mut codec = Codec::h264(hw.then_some(Hw::Vaapi)).expect("decoder");
            codec.copy_ns = Some(Vec::new());
            codec.card_gives_i420 = i420;
            for (sent, which) in rx {
                let began = Instant::now();
                let mut made = 0;
                codec.parse(&all[which], |c, unit| {
                    c.decode(unit, |_f| made += 1).unwrap();
                });
                let arrived = Instant::now();
                let copy_ns: u64 = codec.copy_ns.as_mut().map_or(0, |v| v.drain(..).map(|t| t.0).sum());
                if made > 0 {
                    done_tx.send(Record { sent, arrived, decode_ns: (arrived - began).as_nanos() as u64, copy_ns }).unwrap();
                }
            }
        })
    };
    let start = Instant::now() + Duration::from_millis(50);
    for i in 0..count {
        let due = start + INTERVAL * i as u32;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        tx.send((Instant::now(), i % all.len())).unwrap();
    }
    drop(tx);
    let mut out = Vec::new();
    while let Ok(r) = done_rx.recv_timeout(Duration::from_secs(5)) {
        out.push(r);
        if out.len() >= count {
            break;
        }
    }
    let _ = worker.join();
    out
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p) as usize]
}

#[test]
#[ignore = "a measurement, needs the clips and a VA-API card"]
fn measure() {
    let clip = env_or("BENCH_CLIP", "p1080.h264");
    let seconds: u64 = env_or("BENCH_SECONDS", "20").parse().unwrap();
    let label = env_or("BENCH_LABEL", "idle");
    // The clips come out of the helper as units split at each delimiter, half of them with no picture: join them up to one picture each
    let mut joined: Vec<Vec<u8>> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    for unit in packets(&clip).expect("clip") {
        pending.extend_from_slice(&unit);
        let has_slice = pending.windows(4).any(|w| w[..3] == [0, 0, 1] && matches!(w[3] & 31, 1 | 5));
        if has_slice {
            joined.push(std::mem::take(&mut pending));
        }
    }
    println!("BENCH {clip}: {} pictures, {} bytes", joined.len(), joined.iter().map(Vec::len).sum::<usize>());
    for mode in env_or("BENCH_MODES", "sw,i420,nv12").split(',') {
        let mut recs = run(mode, &joined, seconds);
        recs.sort_by_key(|r| r.arrived);
        let total = (seconds * 30) as usize;
        let body = &recs[30.min(recs.len())..];
        let ms = |ns: u64| ns as f64 / 1e6;
        let mut dec: Vec<f64> = body.iter().map(|r| ms(r.decode_ns)).collect();
        let mut copy: Vec<f64> = body.iter().map(|r| ms(r.copy_ns)).collect();
        let mut gaps: Vec<f64> = body.windows(2).map(|w| (w[1].arrived - w[0].arrived).as_secs_f64() * 1000.0).collect();
        let stutter = gaps.iter().filter(|g| **g > 50.0).count();
        // Pictures a newest-wins hand-over would have dropped: still not out two intervals after their packet came in
        let late = body.iter().filter(|r| (r.arrived - r.sent) > INTERVAL * 2).count();
        println!(
            "BENCH {label:6} {clip:11} {mode:5} | decode p50 {:5.1} p95 {:5.1} p99 {:5.1} max {:5.1} | copy p50 {:5.1} p99 {:5.1} | gap p99 {:5.1} max {:5.1} | gaps>50ms {stutter} late {late} missing {} of {total}",
            pct(&mut dec, 0.5), pct(&mut dec, 0.95), pct(&mut dec, 0.99), pct(&mut dec, 1.0),
            pct(&mut copy, 0.5), pct(&mut copy, 0.99),
            pct(&mut gaps, 0.99), pct(&mut gaps, 1.0),
            total - recs.len().min(total),
        );
    }
}
