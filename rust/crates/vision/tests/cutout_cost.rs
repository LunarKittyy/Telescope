//! What the high quality cutout costs in the app's own path: 1080p I420 frames at 30 a second go through VisionHost::person_mask_hq, as a frame
//! step would ask, with the real model. Ignored: it needs TELESCOPE_MATTING_PACK (an unpacked matting download) and TELESCOPE_MATTING_CLIP
//! (a video), and means little in a debug build. Run it with
//! `cargo test -p telescope-vision --release --test cutout_cost -- --ignored --nocapture`.

use std::path::PathBuf;
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::vision::VisionService;
use telescope_vision::pack::PackInfo;
use telescope_vision::service::LoadError;
use telescope_vision::VisionHost;

const W: usize = 1920;
const H: usize = 1080;

fn clip_frames(count: usize) -> Vec<Frame> {
    let clip = std::env::var("TELESCOPE_MATTING_CLIP").expect("TELESCOPE_MATTING_CLIP is a video");
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i", &clip, "-vf", &format!("scale={W}:{H}"), "-frames:v", &count.to_string(), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout.chunks_exact(W * H * 3 / 2).enumerate().map(|(i, data)| {
        let mut frame = Frame::new(W as u32, H as u32, PixelFormat::I420, data.to_vec());
        frame.number = i as u64;
        frame
    }).collect()
}

/// User plus system CPU seconds this process has used.
fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let after = &stat[stat.rfind(')').unwrap() + 2..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the line, which are 11 and 12 after the name
    (fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap()) / 100.0
}

#[test]
#[ignore = "needs the real model and library (TELESCOPE_MATTING_PACK) and a clip (TELESCOPE_MATTING_CLIP)"]
fn the_cutout_at_1080p_and_30_frames_a_second() {
    let pack: PathBuf = std::env::var("TELESCOPE_MATTING_PACK").expect("TELESCOPE_MATTING_PACK").into();
    let folder = pack.clone();
    let find: telescope_vision::service::Find = Box::new(move || telescope_vision::matting::load(&folder));
    let none: telescope_vision::service::Find = Box::new(|| Err(LoadError::Missing("no vision pack".into())));
    let host = VisionHost::with(none, Some(Box::new(|| false)), pack.clone(), PackInfo::default(), Box::new(telescope_vision::pack::web))
        .with_cutout(find, None, pack, PackInfo::default());
    let frames = clip_frames(120);
    assert_eq!(frames.len(), 120);
    let step = Duration::from_secs_f64(1.0 / 30.0);
    let (mut slowest, mut first_at, mut masks) = (Duration::ZERO, None, 0u32);
    let mut cpu_start = 0.0;
    let begin = Instant::now();
    for (i, frame) in frames.iter().cycle().take(360).enumerate() {
        let due = begin + step * i as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let mut frame = frame.clone();
        frame.number = i as u64;
        let t = Instant::now();
        let mask = host.person_mask_hq("phone", &frame, frame.width as usize, frame.height as usize, 0);
        slowest = slowest.max(t.elapsed());
        if let Some(mask) = mask {
            assert_eq!(mask.len(), W * H);
            first_at.get_or_insert(i);
            masks += 1;
        }
        // 2 seconds in, with the model loaded and warm
        if i == 60 {
            cpu_start = cpu_seconds();
        }
    }
    let used = cpu_seconds() - cpu_start;
    let seconds = (360 - 60) as f64 / 30.0;
    println!("first mask at frame {first_at:?}, {masks} of 360 frames had one, the slowest ask took {slowest:?}");
    println!("CPU: {used:.2} s over {seconds:.1} s of stream = {:.2} cores (this process, all threads, with the test's own frame copies)", used / seconds);
    for (name, line) in host.diagnostics() {
        println!("{name}: {line}");
    }
    assert!(first_at.is_some());
    assert!(slowest < Duration::from_millis(100), "no frame is held up: {slowest:?}");
    host.shutdown();
}
