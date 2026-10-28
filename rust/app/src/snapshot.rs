//! `telescope-rs snapshot OUT`: the window in the same state as the Qt reference picture (desktop/scripts/
//! translation_scenes.main_window: Pixel 8 paired, not streaming), written as a PPM once it has drawn.

use crate::gui::MainWindow;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

pub fn prepare(ui: &MainWindow) {
    ui.set_phones(ModelRc::new(VecModel::from(vec![SharedString::from("Pixel 8")])));
    ui.set_current_phone(0);
    ui.set_camera_active(true);
    ui.window().set_size(slint::LogicalSize::new(1300.0, 780.0));
}

pub fn save(ui: &MainWindow, out: &str) {
    match ui.window().take_snapshot() {
        Ok(buf) => {
            let mut ppm = format!("P6\n{} {}\n255\n", buf.width(), buf.height()).into_bytes();
            for px in buf.as_slice() {
                ppm.extend_from_slice(&[px.r, px.g, px.b]);
            }
            match std::fs::write(out, ppm) {
                Ok(()) => println!("{}x{} at scale {} -> {out}", buf.width(), buf.height(), ui.window().scale_factor()),
                Err(e) => eprintln!("{out}: {e}"),
            }
        }
        Err(e) => eprintln!("snapshot failed: {e}"),
    }
}
