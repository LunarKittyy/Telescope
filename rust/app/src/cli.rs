//! The window-less modes: pairing from a terminal, a headless stream and the config snapshot, for testing and measuring.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telescope_core::config::ConfigFile;
use telescope_core::link::{PhoneSession, RoutePrefs, StreamRequest};
use telescope_core::services::Services;
use telescope_core::source::Phones;
use telescope_core::Value;
use telescope_shell::flow::{Stats, StreamEvents, StreamOptions, Streaming};
use telescope_shell::WindowOptions;

pub struct Args {
    pub usb: bool,
    pub package: String,
    pub device: Option<String>,
    pub vaapi: bool,
    pub seconds: u64,
    pub route: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub autostart: bool,
    pub quit_after: Option<u64>,
}

impl Args {
    fn prefs(&self) -> RoutePrefs {
        RoutePrefs { usb: self.route != "wifi", network: self.route != "usb" }
    }

    fn request(&self) -> StreamRequest {
        StreamRequest { width: self.width, height: self.height, fps: self.fps, camera_off: false }
    }

    pub fn window(&self, snapshot: Option<String>, minimized: bool, on_shown: Option<telescope_shell::ShownHook>) -> WindowOptions {
        WindowOptions {
            prefs: self.prefs(),
            request: self.request(),
            hardware_decode: self.vaapi,
            package: self.package.clone(),
            autostart: self.autostart,
            quit_after: self.quit_after,
            snapshot,
            minimized,
            on_shown,
        }
    }
}

fn load_phones(services: &Services) -> (ConfigFile, Phones) {
    let file = ConfigFile::default();
    let cfg = file.load();
    let phones = Phones::from_section(cfg.pointer("/plugin_configs/connection"), || services.link.new_identity());
    (file, phones)
}

pub fn pair(args: &Args, services: &Services) -> Result<(), String> {
    let (file, mut phones) = load_phones(services);
    let offer = services.link.pair(&phones.computer_id, &phones.computer_name)?;
    let code = qrcode::QrCode::new(offer.payload().as_bytes()).map_err(|e| e.to_string())?;
    println!("{}", code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build());
    println!("Scan this with the Telescope app (port {}), or wait for the USB offer.", offer.port());
    if args.usb {
        println!("USB offer to {}: {} phone(s) took it", args.package, offer.offer_over_usb(&args.package));
    }
    let until = Instant::now() + Duration::from_secs(120);
    let phone = loop {
        if let Some(p) = offer.poll() {
            break p;
        }
        if Instant::now() > until {
            return Err("No phone paired within 2 minutes".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    println!("Paired with {} ({})", phone.name, phone.id);
    phones.add_phone(phone);
    let saved = file.update(|cfg| {
        if let Some(pcfg) = cfg.get_mut("plugin_configs").and_then(Value::as_object_mut) {
            pcfg.insert("connection".into(), phones.to_section());
        }
        cfg["selected_device"] = phones.selected_phone.clone().map_or(Value::Null, Value::String);
    });
    if saved { Ok(()) } else { Err(format!("Couldn't save {}", file.path().display())) }
}

struct Printer;

impl StreamEvents for Printer {
    fn preview(&self, _rgb: Vec<u8>, _w: u32, _h: u32) {}
    fn state(&self, _state: Value) {}
    fn stats(&self, s: Stats) {
        println!("{}x{} {:.1} fps {:.1} Mbps decode {:.2} ms/frame {}{} {}",
                 s.width, s.height, s.fps, s.mbps, s.decode_ms, if s.hardware { "VA-API " } else { "" }, s.camera, s.error);
    }
    fn ended(&self, why: String) {
        eprintln!("Stream ended: {why}");
    }
}

fn session_for(args: &Args, services: &Services) -> Result<Arc<dyn PhoneSession>, String> {
    let (_, phones) = load_phones(services);
    let phone = phones.selected().ok_or("No phone paired yet: run `telescope-rs pair` first")?;
    if args.route != "usb" {
        std::thread::sleep(Duration::from_millis(1500)); // give mDNS a moment to hear the phone
    }
    services.link.resolve(phone, args.prefs()).map(Arc::from).map_err(|e| format!("Can't reach {}: {e:?}", phone.name))
}

pub fn stream(args: &Args, services: &Services) -> Result<(), String> {
    let started = Instant::now();
    let session = session_for(args, services)?;
    println!("Route: {} (found in {} ms)", session.describe(), started.elapsed().as_millis());
    session.start(args.request())?;
    println!("Phone streaming after {} ms", started.elapsed().as_millis());
    let opts = StreamOptions { fps: args.fps, slot: 0, hardware_decode: args.vaapi };
    let streaming = Streaming::start(session.clone(), services.media.clone(), services.outputs.clone(), opts, Arc::new(Printer));
    let until = Instant::now() + Duration::from_secs(args.seconds);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
    }
    streaming.stop();
    let _ = session.stop();
    session.release();
    Ok(())
}

/// Sends one /v1/control body to the streaming phone and prints what /v1/state says afterwards for its action.
pub fn control(args: &Args, services: &Services, body: &str) -> Result<(), String> {
    let session = session_for(args, services)?;
    let body: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let action = body.get("action").and_then(|a| a.as_str()).unwrap_or("").to_string();
    let before = session.state()?;
    session.send(&body)?;
    std::thread::sleep(Duration::from_millis(700));
    let after = session.state()?;
    println!("{action}: {} -> {}", before.get(&action).unwrap_or(&Value::Null), after.get(&action).unwrap_or(&Value::Null));
    session.release();
    Ok(())
}

/// Prints what this app makes of a config fixture folder, in the shape of its expected.json.
pub fn config_snapshot(folder: Option<&str>) -> Result<(), String> {
    let folder = Path::new(folder.ok_or("Usage: telescope-rs config-snapshot <fixture folder>")?);
    if !folder.is_dir() {
        return Err(format!("{} isn't a folder", folder.display()));
    }
    let scratch = std::env::temp_dir().join(format!("telescope-rs-config-snapshot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let result = telescope_core::snapshot::snapshot(folder, &scratch, &[], &[]);
    let _ = std::fs::remove_dir_all(&scratch);
    print!("{}", telescope_core::snapshot::dump(&result.map_err(|e| e.to_string())?));
    Ok(())
}
