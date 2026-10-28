//! The window-less modes: pairing from a terminal and a headless stream, for testing and measuring.

use base64::Engine;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telescope_core::config::Config;
use telescope_core::discovery::LanDiscovery;
use telescope_core::pairing::{PairingServer, PAIRING_PORT};
use telescope_core::stream::{Stats, StreamEvents, StreamOptions, Streaming};
use telescope_core::{adb, session};

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

pub fn pair(args: &Args) -> Result<(), String> {
    let mut cfg = Config::load();
    let server = PairingServer::start(&cfg.computer_id, &cfg.computer_name).map_err(|e| e.to_string())?;
    let code = qrcode::QrCode::new(server.offer.payload.as_bytes()).map_err(|e| e.to_string())?;
    println!("{}", code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build());
    println!("Scan this with the Telescope app (port {}), or wait for the USB offer.", server.offer.port);
    let mut reversed = Vec::new();
    if args.usb {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&server.offer.usb_payload);
        for serial in adb::devices() {
            if server.offer.port == PAIRING_PORT && adb::reverse(&serial, server.offer.port) {
                reversed.push(serial.clone());
            }
            let sent = adb::broadcast_pair(&serial, &args.package, &b64);
            println!("USB offer to {serial} ({}): {}", args.package, if sent { "sent" } else { "failed" });
        }
    }
    let phone = server.paired.recv_timeout(Duration::from_secs(120)).map_err(|_| "No phone paired within 2 minutes")?;
    for serial in reversed {
        adb::unreverse(&serial, server.offer.port);
    }
    println!("Paired with {} ({})", phone.name, phone.id);
    cfg.add_phone(phone);
    cfg.save().map_err(|e| e.to_string())
}

struct Printer;

impl StreamEvents for Printer {
    fn preview(&self, _rgb: Vec<u8>, _w: u32, _h: u32) {}
    fn state(&self, _state: serde_json::Value) {}
    fn stats(&self, s: Stats) {
        println!("{}x{} {:.1} fps {:.1} Mbps decode {:.2} ms/frame {}{} {}",
                 s.width, s.height, s.fps, s.mbps, s.decode_ms, if s.hardware { "VA-API " } else { "" }, s.camera, s.error);
    }
    fn ended(&self, why: String) {
        eprintln!("Stream ended: {why}");
    }
}

pub fn route_for(cfg: &Config, route: &str) -> Result<session::Route, String> {
    let phone = cfg.selected().ok_or("No phone paired yet: run `telescope-rs pair` first")?;
    let lan = (route != "usb").then(LanDiscovery::start);
    if lan.is_some() {
        std::thread::sleep(Duration::from_millis(1500)); // give mDNS a moment to hear the phone
    }
    session::resolve(phone, lan.as_ref(), route != "wifi", route != "usb").map_err(|e| format!("Can't reach {}: {e:?}", phone.name))
}

pub fn stream(args: &Args) -> Result<(), String> {
    let cfg = Config::load();
    let started = Instant::now();
    let route = route_for(&cfg, &args.route)?;
    println!("Route: {} (found in {} ms)", route.describe(), started.elapsed().as_millis());
    route.start(args.width, args.height, args.fps)?;
    println!("Phone streaming after {} ms", started.elapsed().as_millis());
    let opts = StreamOptions { width: args.width, height: args.height, fps: args.fps, device: args.device.clone(), vaapi: args.vaapi };
    let streaming = Streaming::start(route.clone(), opts, Arc::new(Printer));
    let until = Instant::now() + Duration::from_secs(args.seconds);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
    }
    streaming.stop();
    let _ = route.stop();
    route.release();
    Ok(())
}

/// Sends one /v1/control body to the streaming phone and prints what /v1/state says afterwards for its action.
pub fn control(args: &Args, body: &str) -> Result<(), String> {
    let cfg = Config::load();
    let route = route_for(&cfg, &args.route)?;
    let body: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let action = body.get("action").and_then(|a| a.as_str()).unwrap_or("").to_string();
    let before = route.state().map_err(|e| e.to_string())?;
    route.control(&body).map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(700));
    let after = route.state().map_err(|e| e.to_string())?;
    println!("{action}: {} -> {}", before.get(&action).unwrap_or(&serde_json::Value::Null), after.get(&action).unwrap_or(&serde_json::Value::Null));
    route.release();
    Ok(())
}
