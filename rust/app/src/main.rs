//! telescope-rs: a prototype of the Telescope desktop app in Rust and Slint.
//!
//!   telescope-rs                 the window
//!   telescope-rs pair [--usb]    pair a phone from the terminal (QR code, or the USB offer over adb)
//!   telescope-rs stream          stream without a window, printing stats each second
//!   telescope-rs control JSON    send one /v1/control body to a streaming phone and show the state it changed
//!   telescope-rs snapshot OUT    save the window as a PPM once it has drawn, for comparing with the Qt one
//!
//! --autostart starts streaming once the window is up and --quit-after SECS closes it, for measuring.

mod cli;
mod gui;
mod snapshot;

fn main() {
    let mut words: Vec<String> = std::env::args().skip(1).collect();
    let flag = |words: &mut Vec<String>, name: &str| -> bool {
        let found = words.iter().position(|w| w == name);
        found.map(|i| words.remove(i)).is_some()
    };
    let value = |words: &mut Vec<String>, name: &str| -> Option<String> {
        let i = words.iter().position(|w| w == name)?;
        words.remove(i);
        (i < words.len()).then(|| words.remove(i))
    };
    let args = cli::Args {
        usb: flag(&mut words, "--usb"),
        vaapi: flag(&mut words, "--vaapi"),
        package: value(&mut words, "--package").unwrap_or_else(|| "com.telescope.dev".into()),
        device: value(&mut words, "--device"),
        seconds: value(&mut words, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(30),
        route: value(&mut words, "--route").unwrap_or_else(|| "auto".into()),
        width: value(&mut words, "--width").and_then(|s| s.parse().ok()).unwrap_or(1920),
        height: value(&mut words, "--height").and_then(|s| s.parse().ok()).unwrap_or(1080),
        fps: value(&mut words, "--fps").and_then(|s| s.parse().ok()).unwrap_or(30),
        autostart: flag(&mut words, "--autostart"),
        quit_after: value(&mut words, "--quit-after").and_then(|s| s.parse().ok()),
    };
    let result = match words.first().map(String::as_str) {
        Some("pair") => cli::pair(&args),
        Some("stream") => cli::stream(&args),
        Some("control") => cli::control(&args, words.get(1).map(String::as_str).unwrap_or("{}")),
        Some("snapshot") => gui::run(args, Some(words.get(1).cloned().unwrap_or_else(|| "snapshot.ppm".into()))),
        Some(other) => Err(format!("Unknown command {other}")),
        None => gui::run(args, None),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
