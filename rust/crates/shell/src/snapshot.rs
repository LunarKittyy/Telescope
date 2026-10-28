//! `telescope-rs snapshot OUT`: the window as it drew, written as a PPM, for comparing with the Qt one by eye.

pub fn save(window: &slint::Window, out: &str) {
    match window.take_snapshot() {
        Ok(buf) => {
            let mut ppm = format!("P6\n{} {}\n255\n", buf.width(), buf.height()).into_bytes();
            for px in buf.as_slice() {
                ppm.extend_from_slice(&[px.r, px.g, px.b]);
            }
            match std::fs::write(out, ppm) {
                Ok(()) => println!("{}x{} at scale {} -> {out}", buf.width(), buf.height(), window.scale_factor()),
                Err(e) => eprintln!("{out}: {e}"),
            }
        }
        Err(e) => eprintln!("snapshot failed: {e}"),
    }
}
