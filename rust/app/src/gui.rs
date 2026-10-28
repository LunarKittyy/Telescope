//! The window: wires MainWindow's callbacks to the core and pushes stream events back onto the UI thread.

use crate::cli::Args;
use base64::Engine;
use serde_json::{json, Value};
use slint::{ComponentHandle, Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::config::Config;
use telescope_core::discovery::LanDiscovery;
use telescope_core::pairing::{PairingServer, PAIRING_PORT};
use telescope_core::session::{self, Route};
use telescope_core::stream::{Stats, StreamEvents, StreamOptions, Streaming};
use telescope_core::adb;

slint::include_modules!();

struct App {
    cfg: Config,
    lan: LanDiscovery,
    streaming: Option<Streaming>,
    route: Option<Route>,
    pairing: Option<PairingServer>,
}

struct Events {
    ui: Weak<MainWindow>,
}

impl StreamEvents for Events {
    fn preview(&self, rgb: Vec<u8>, width: u32, height: u32) {
        let buffer = SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(&rgb, width, height);
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_preview(Image::from_rgb8(buffer));
            ui.set_has_preview(true);
        });
    }

    fn state(&self, state: Value) {
        let _ = self.ui.upgrade_in_event_loop(move |ui| apply_state(&ui, &state));
    }

    fn stats(&self, s: Stats) {
        let text = format!("{}x{} · {:.0} fps · {:.1} Mbps · {:.1} ms decode{}{}", s.width, s.height, s.fps, s.mbps, s.decode_ms,
                           if s.hardware { " (VA-API)" } else { "" },
                           if s.error.is_empty() { format!(" · {}", s.camera) } else { format!(" · {}", s.error) });
        let _ = self.ui.upgrade_in_event_loop(move |ui| ui.set_stats(text.into()));
    }

    fn ended(&self, why: String) {
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_streaming(false);
            ui.set_has_preview(false);
            ui.set_status(why.into());
        });
    }
}

/// A lens label from /v1/state as the tile shows it: "Back ~24mm OIS" is ("24", "mm", "OIS").
fn lens_tile(cam: &Value) -> Lens {
    let label = cam.get("label").and_then(Value::as_str).unwrap_or("");
    let mut number = String::new();
    let mut unit = String::new();
    let mut rest: Vec<String> = Vec::new();
    for word in label.split_whitespace() {
        let w = word.trim_start_matches('~');
        let digits: String = w.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if number.is_empty() && !digits.is_empty() && (w.ends_with("mm") || w.ends_with('x')) {
            number = digits;
            unit = w[number.len()..].to_string();
        } else if word == "[auto]" {
            rest.insert(0, "Auto".into());
        } else if word == "Front" || word.starts_with("F/") {
            rest.insert(0, "Front".into());
        } else if word != "Back" && word != "[phys]" {
            rest.push(word.to_string());
        }
    }
    if number.is_empty() {
        number = cam.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
    }
    Lens {
        id: cam.get("id").and_then(Value::as_str).unwrap_or("").into(),
        number: number.into(),
        unit: unit.into(),
        rest: rest.join(" · ").into(),
    }
}

fn apply_state(ui: &MainWindow, state: &Value) {
    let cams: Vec<Value> = state.get("cameras").and_then(Value::as_array).cloned().unwrap_or_default();
    let lenses: Vec<Lens> = cams.iter().map(lens_tile).collect();
    let current = cams.iter().position(|c| c.get("current").and_then(Value::as_bool) == Some(true)).unwrap_or(0);
    ui.set_lenses(ModelRc::new(VecModel::from(lenses)));
    ui.set_current_lens(current as i32);
    if let Some(cam) = cams.get(current) {
        let i = |k: &str, d: i64| cam.get(k).and_then(Value::as_i64).unwrap_or(d) as i32;
        ui.set_ae_min(i("aeCompMin", -8));
        ui.set_ae_max(i("aeCompMax", 8));
        ui.set_ae_step(cam.get("aeCompStep").and_then(Value::as_f64).unwrap_or(0.167) as f32);
        ui.set_point_focus(cam.get("supportsFocusPoint").and_then(Value::as_bool).unwrap_or(false));
    }
    let b = |k: &str, d: bool| state.get(k).and_then(Value::as_bool).unwrap_or(d);
    let n = |k: &str, d: i64| state.get(k).and_then(Value::as_i64).unwrap_or(d) as i32;
    ui.set_exposure_mode(if b("auto", true) { 0 } else { 1 });
    ui.set_ae_comp(n("ae_comp", 0) as f32);
    ui.set_ois(b("ois", true));
    ui.set_black_level_lock(b("black_level_lock", false));
    ui.set_torch(b("torch", false));
    ui.set_noise_reduction(n("nr_mode", 1).clamp(0, 2));
    ui.set_sharpening(n("edge_mode", 1).clamp(0, 2));
    match state.get("focus_mode").and_then(Value::as_str) {
        Some("manual") => ui.set_focus_mode(2),
        Some("point") => ui.set_focus_mode(1),
        _ => ui.set_focus_mode(0),
    }
}

fn refresh_phones(ui: &MainWindow, cfg: &Config) {
    let names: Vec<SharedString> = cfg.phones.iter().map(|p| p.name.as_str().into()).collect();
    let current = cfg.selected().and_then(|s| cfg.phones.iter().position(|p| p.id == s.id)).unwrap_or(0);
    ui.set_phones(ModelRc::new(VecModel::from(names)));
    ui.set_current_phone(current as i32);
    ui.set_camera_active(!cfg.phones.is_empty());
}

fn qr_image(payload: &str) -> Image {
    let Ok(code) = qrcode::QrCode::new(payload.as_bytes()) else { return Image::default() };
    let w = code.width();
    let scale = 4;
    let size = ((w + 4) * scale) as u32;
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(size, size);
    let colors = code.to_colors();
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % size as usize / scale, i / size as usize / scale);
        let dark = x >= 2 && y >= 2 && x < w + 2 && y < w + 2 && colors[(y - 2) * w + (x - 2)] == qrcode::Color::Dark;
        *px = if dark { Rgb8Pixel::new(0, 0, 0) } else { Rgb8Pixel::new(255, 255, 255) };
    }
    Image::from_rgb8(buf)
}

pub fn run(args: Args, snapshot: Option<String>) -> Result<(), String> {
    let ui = MainWindow::new().map_err(|e| e.to_string())?;
    let app = Arc::new(Mutex::new(App { cfg: Config::load(), lan: LanDiscovery::start(), streaming: None, route: None, pairing: None }));
    refresh_phones(&ui, &app.lock().unwrap().cfg);
    let args = Arc::new(args);

    ui.on_phone_picked({
        let app = app.clone();
        move |i| {
            let mut a = app.lock().unwrap();
            a.cfg.selected_phone = a.cfg.phones.get(i as usize).map(|p| p.id.clone());
            let _ = a.cfg.save();
        }
    });

    ui.on_start_stop({
        let (app, weak, args) = (app.clone(), ui.as_weak(), args.clone());
        move || {
            let ui = weak.unwrap();
            let running = app.lock().unwrap().streaming.take();
            if let Some(streaming) = running {
                let route = app.lock().unwrap().route.take();
                ui.set_streaming(false);
                ui.set_has_preview(false);
                ui.set_status("Not streaming".into());
                ui.set_stats("".into());
                std::thread::spawn(move || {
                    streaming.stop();
                    if let Some(r) = route {
                        let _ = r.stop();
                        r.release();
                    }
                });
                return;
            }
            ui.set_starting(true);
            ui.set_status("Looking for the phone…".into());
            let (app, weak, args) = (app.clone(), weak.clone(), args.clone());
            std::thread::spawn(move || {
                let phone = app.lock().unwrap().cfg.selected().cloned();
                let result = phone.ok_or_else(|| "No phone".to_string()).and_then(|phone| {
                    let a = app.lock().unwrap();
                    let lan = (args.route != "usb").then_some(&a.lan);
                    let route = session::resolve(&phone, lan, args.route != "wifi", args.route != "usb")
                        .map_err(|e| format!("Can't reach {}: {e:?}", phone.name))?;
                    drop(a);
                    route.start(args.width, args.height, args.fps)?;
                    Ok(route)
                });
                let ui_weak = weak.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_starting(false);
                    match result {
                        Ok(route) => {
                            let opts = StreamOptions { width: args.width, height: args.height, fps: args.fps,
                                                       device: args.device.clone(), vaapi: args.vaapi };
                            ui.set_status(format!("Streaming over {}", route.describe()).into());
                            ui.set_streaming(true);
                            let streaming = Streaming::start(route.clone(), opts, Arc::new(Events { ui: ui_weak }));
                            let scale = ui.window().scale_factor();
                            streaming.set_preview_size((ui.get_preview_width() * scale) as u32, (ui.get_preview_height() * scale) as u32);
                            let mut a = app.lock().unwrap();
                            a.route = Some(route);
                            a.streaming = Some(streaming);
                        }
                        Err(e) => ui.set_status(e.into()),
                    }
                });
            });
        }
    });

    ui.on_control({
        let app = app.clone();
        move |action, value| {
            let Some(route) = app.lock().unwrap().route.clone() else { return };
            let body = match action.as_str() {
                "focus_mode" => json!({"action": "focus_mode", "value": if value > 0.5 { "manual" } else { "continuous" }}),
                "focus_distance" => json!({"action": "focus_distance", "value": value}),
                "auto" | "wb_auto" => json!({"action": action.as_str()}),
                _ => json!({"action": action.as_str(), "value": value.round() as i64}),
            };
            std::thread::spawn(move || {
                let _ = route.control(&body);
            });
        }
    });

    ui.on_manual_exposure({
        let app = app.clone();
        move |iso, shutter_ns| {
            let Some(route) = app.lock().unwrap().route.clone() else { return };
            std::thread::spawn(move || {
                let _ = route.control(&json!({"action": "iso", "value": iso.round() as i64}));
                let _ = route.control(&json!({"action": "shutter", "value": shutter_ns.round() as i64}));
            });
        }
    });

    ui.on_lens_picked({
        let app = app.clone();
        move |id| {
            let Some(route) = app.lock().unwrap().route.clone() else { return };
            std::thread::spawn(move || {
                let _ = route.control(&json!({"action": "camera", "id": id.as_str()}));
            });
        }
    });

    ui.on_add_phone({
        let (app, weak) = (app.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let mut a = app.lock().unwrap();
            if a.pairing.is_none() {
                match PairingServer::start(&a.cfg.computer_id, &a.cfg.computer_name) {
                    Ok(server) => {
                        ui.set_pairing_qr(qr_image(&server.offer.payload));
                        let ips: Vec<&str> = server.offer.candidates.iter().map(|c| c.ip.as_str()).collect();
                        ui.set_pairing_note(format!("Listening on {} port {}", ips.join(", "), server.offer.port).into());
                        a.pairing = Some(server);
                    }
                    Err(e) => ui.set_pairing_note(format!("Couldn't start pairing: {e}").into()),
                }
            }
            ui.set_pairing_open(true);
        }
    });

    ui.on_pair_usb({
        let (app, weak, args) = (app.clone(), ui.as_weak(), args.clone());
        move || {
            let Some((payload, port)) = app.lock().unwrap().pairing.as_ref().map(|p| (p.offer.usb_payload.clone(), p.offer.port)) else { return };
            let (weak, package) = (weak.clone(), args.package.clone());
            std::thread::spawn(move || {
                let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
                let mut sent = 0;
                for serial in adb::devices() {
                    if port == PAIRING_PORT {
                        adb::reverse(&serial, port);
                    }
                    sent += adb::broadcast_pair(&serial, &package, &b64) as usize;
                }
                let note = if sent > 0 { "Sent the offer over USB: accept it on the phone" } else { "No phone answered over USB" };
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_pairing_note(note.into()));
            });
        }
    });

    ui.on_close_pairing({
        let (app, weak) = (app.clone(), ui.as_weak());
        move || {
            app.lock().unwrap().pairing = None;
            weak.unwrap().set_pairing_open(false);
        }
    });

    // Picks up a finished pairing and keeps the stream's preview the size the window shows it at
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), {
        let (app, weak) = (app.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut a = app.lock().unwrap();
            if let Some(phone) = a.pairing.as_ref().and_then(|p| p.paired.try_recv().ok()) {
                a.cfg.add_phone(phone);
                let _ = a.cfg.save();
                a.pairing = None;
                refresh_phones(&ui, &a.cfg);
                ui.set_pairing_open(false);
            }
            if let Some(s) = a.streaming.as_ref() {
                let scale = ui.window().scale_factor();
                s.set_preview_size((ui.get_preview_width() * scale) as u32, (ui.get_preview_height() * scale) as u32);
            }
        }
    });

    if let Some(out) = snapshot {
        crate::snapshot::prepare(&ui);
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1500), move || {
            let ui = weak.unwrap();
            crate::snapshot::save(&ui, &out);
            let _ = slint::quit_event_loop();
        });
    }

    ui.run().map_err(|e| e.to_string())?;
    if let Some(s) = app.lock().unwrap().streaming.take() {
        s.stop();
    }
    if let Some(r) = app.lock().unwrap().route.take() {
        let _ = r.stop();
        r.release();
    }
    Ok(())
}
