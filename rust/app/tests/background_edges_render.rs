//! Renders Background's edge shaping on real frames with the real person model, to judge the mask by eye. Does nothing unless
//! TELESCOPE_EDGE_FRAMES (a folder of 1280x720 .rgb files) and TELESCOPE_EDGE_OUT (where the .ppm files go) are set.

use telescope_effects::cpu::Rgb;
use telescope_effects::imgops::{compose, Gray};
use telescope_vision::ops::{self, Rgb as VRgb};
use telescope_vision::service::Backend;

const VARIANTS: [(&str, i64, i64, u8, u8); 6] = [
    ("a-before", 3, 0, 0, 255),
    ("b-firm-40-170", 3, 0, 40, 170),
    ("c-firm-30-130", 3, 0, 30, 130),
    ("d-firm-60-150", 3, 0, 60, 150),
    ("e-firm-20-110", 3, 0, 20, 110),
    ("f-firm-50-190", 3, 0, 50, 190),
];

fn ppm(path: &std::path::Path, rgb: &Rgb) {
    let mut out = format!("P6\n{} {}\n255\n", rgb.w, rgb.h).into_bytes();
    out.extend_from_slice(&rgb.data);
    std::fs::write(path, out).unwrap();
}

#[test]
fn render_the_edge_variants() {
    let (Some(frames), Some(out)) = (std::env::var_os("TELESCOPE_EDGE_FRAMES"), std::env::var_os("TELESCOPE_EDGE_OUT")) else { return };
    let (dir, info) = (telescope_vision::pack::default_dir(), telescope_vision::pack::PackInfo::current());
    assert!(info.installed(&dir), "no vision pack");
    let backend = telescope_vision::mediapipe::MediaPipe::load(&info.folder(&dir)).unwrap_or_else(|e| panic!("{e:?}"));
    let out = std::path::PathBuf::from(out);
    for entry in std::fs::read_dir(frames).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rgb") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let data = std::fs::read(&path).unwrap();
        let (w, h) = (1280, 720);
        let frame = VRgb::new(w, h, data.clone());
        let small = ops::model_input(&frame, ops::MODEL_SIZE).unwrap();
        let mut segment = backend.segmenter().unwrap();
        let mask = segment.segment(&small, 1).unwrap();
        let wide = Gray { w, h, data: ops::mask_resized(&mask.data, mask.w, mask.h, w, h) };
        let mid = wide.data.iter().filter(|&&v| (40..215).contains(&v)).count();
        let some = wide.data.iter().filter(|&&v| v >= 40).count();
        eprintln!("{name}: {:.1}% of the person is between 40 and 214", 100.0 * mid as f64 / some.max(1) as f64);
        let top = Rgb { w, h, data };
        let backdrop = Rgb { w, h, data: (0..w * h).flat_map(|_| [255u8, 0, 200]).collect() };
        for (label, edges, cut, low, high) in VARIANTS {
            let soft = wide.firm(low, high).soften_cut(edges, cut);
            ppm(&out.join(format!("{name}-{label}.ppm")), &compose(&top, &soft, &backdrop));
        }
    }
}
