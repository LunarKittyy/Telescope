use super::*;
use telescope_core::testing::FakeHost;

fn plugin() -> StreamOutputPlugin {
    StreamOutputPlugin::new(true)
}

fn state(sizes: &[(u32, u32)], live: Option<(u32, u32)>) -> Value {
    let list: Vec<Value> = sizes.iter().map(|(w, h)| json!({"width": w, "height": h})).collect();
    let mut s = json!({"codecs": ["h264", "mjpeg"], "codec": "h264",
                       "cameras": [{"id": "0", "current": true, "maxFps": 60, "supportedSizes": list, "h264Sizes": list}]});
    if let Some((w, h)) = live {
        s["stream_width"] = json!(w);
        s["stream_height"] = json!(h);
    }
    s
}

fn started(p: &mut StreamOutputPlugin, host: &mut FakeHost) {
    p.on_stream_start(host, &StreamInfo { source_id: "phone1".into(), url: "https://192.168.1.5:8443/v1/video.h264".into() });
}

#[test]
fn sizes_group_by_shape_and_snap_near_ratios() {
    assert_eq!(aspect_ratio(854, 480), (16, 9));
    assert_eq!(aspect_ratio(1440, 1080), (4, 3));
    assert_eq!(aspect_ratio(1080, 1080), (1, 1));
    assert_eq!(closest_size((1280, 720), [(1920, 1080), (640, 480), (1280, 960)]), Some((1920, 1080)));
    assert_eq!(parse_size_label("1280 x 720"), Some((1280, 720)));
    assert_eq!(parse_size_label("1280 × 720"), Some((1280, 720)));
    assert_eq!(parse_size_label("0 x 720"), None);
    assert_eq!(parse_size_label("big"), None);
}

#[test]
fn a_new_phone_with_nothing_saved_gets_1080p_and_tells_the_phone() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    started(&mut p, &mut host);
    p.on_phone_state(&mut host, &state(&[(3840, 2160), (1920, 1080), (1280, 720), (640, 480)], Some((640, 480))));
    assert_eq!(p.sizes.current(), Some((1920, 1080)));
    assert!(host.calls.iter().any(|c| c.contains(r#""action":"resolution","width":1920,"height":1080"#)), "{:?}", host.calls);
    assert!(host.events.contains(&Event::ResolutionChangeRequested { width: 1920, height: 1080 }));
    assert_eq!(p.get_config()["resolution"], "1920 x 1080");
}

#[test]
fn a_saved_size_is_picked_when_the_phone_reports_its_sizes() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    p.set_config(&json!({"resolution": "1280 x 720", "fps": 24, "format": "mjpeg"})).unwrap();
    assert_eq!(p.opening(), json!({"fps": 24, "width": 1280, "height": 720}));
    started(&mut p, &mut host);
    p.on_phone_state(&mut host, &state(&[(1920, 1080), (1280, 720)], Some((1280, 720))));
    assert_eq!(p.sizes.current(), Some((1280, 720)));
    assert_eq!(p.stream_format(), FORMAT_MJPEG);
    assert!(!host.calls.iter().any(|c| c.contains("resolution")), "the phone already runs at it");
}

#[test]
fn the_settings_round_trip_the_way_older_versions_read_them() {
    let mut p = plugin();
    p.set_config(&json!({"resolution": "1920 × 1080", "fps": 27, "jpeg_quality": 70, "format": "h264", "bitrate_mbps": -1})).unwrap();
    assert_eq!(p.get_config(), json!({"fps": 25, "jpeg_quality": 70, "format": "h264", "bitrate_mbps": -1, "resolution": "1920 x 1080"}));
    p.set_config(&json!({"bitrate_mbps": 500})).unwrap();
    assert_eq!(p.get_config()["bitrate_mbps"], 100);
    assert!(p.get_config().get("resolution").is_none());
}

#[test]
fn a_phone_without_an_encoder_goes_heavy_and_reconnects() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    let mut s = state(&[(1280, 720)], None);
    s["codecs"] = json!(["mjpeg"]);
    p.on_phone_state(&mut host, &s);
    assert_eq!(p.stream_format(), FORMAT_MJPEG);
    assert!(host.calls.contains(&"reconnect_stream".to_string()));
    assert_eq!(p.props()["h264-enabled"], false);
    assert_eq!(p.props()["light-tip"], "H.264. This phone has no H.264 encoder.");
}

#[test]
fn too_much_for_the_encoder_stops_and_offers_the_last_size_that_worked() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    host.focused = Some("phone1".into());
    started(&mut p, &mut host);
    p.on_phone_state(&mut host, &state(&[(3840, 2160), (1920, 1080)], Some((1920, 1080))));
    let mut s = state(&[(3840, 2160), (1920, 1080)], Some((3840, 2160)));
    s["codec_error"] = json!("Too big");
    s["codec_unsupported"] = json!(true);
    s["codec_error_code"] = json!("h264_unavailable");
    s["codec_error_width"] = json!(3840);
    s["codec_error_height"] = json!(2160);
    p.on_phone_state(&mut host, &s);
    assert!(host.calls.iter().any(|c| c == "show_issue encoder Too much for the phone's H.264 encoder"));
    assert!(host.is_scheduled(T_STOP));
    assert_eq!(p.saved_resolution_text.as_deref(), Some("1920 × 1080"));
    assert_eq!(codec_error_text(&s), "H.264 isn't available at 3840x2160 on this phone");
    p.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: T_STOP.into() });
    assert!(host.calls.iter().any(|c| c.starts_with("stop_stream")), "{:?}", host.calls);
}

#[test]
fn the_phone_going_back_to_mjpeg_switches_to_heavy_with_a_banner() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    let mut s = state(&[(1280, 720)], None);
    s["codec_error"] = json!("Encoder failed");
    p.on_phone_state(&mut host, &s);
    assert!(host.calls.iter().any(|c| c == "show_issue h264 Switched to Heavy"));
    assert_eq!(p.stream_format(), FORMAT_MJPEG);
}

#[test]
fn falling_behind_suggests_what_helps_with_the_wifi_hint_first() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    host.focused = Some("phone1".into());
    let mut s = state(&[(1280, 720)], None);
    s["wifi_band"] = json!("2.4");
    s["dynamic_bitrate"] = json!(true);
    p.on_phone_state(&mut host, &s);
    p.on_event(&mut host, &Event::StreamBehind { source_id: "phone1".into(), behind: true });
    assert!(host.calls.iter().any(|c| c == "show_issue behind Can't keep up"));
    p.on_issue_action(&mut host, "behind", A_DYNAMIC);
    assert!(p.dynamic());
    p.on_event(&mut host, &Event::StreamBehind { source_id: "phone1".into(), behind: false });
    assert!(host.calls.iter().any(|c| c == "clear_issue Some(\"behind\")"));
}

#[test]
fn a_lens_slower_than_the_picked_rate_runs_its_fastest_and_keeps_the_pick() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    p.set_config(&json!({"fps": 60})).unwrap();
    started(&mut p, &mut host);
    let mut s = state(&[(1280, 720)], Some((1280, 720)));
    s["cameras"][0]["maxFps"] = json!(30);
    p.on_phone_state(&mut host, &s);
    assert_eq!(p.fps(), 30);
    assert_eq!(p.get_config()["fps"], 60);
    assert!(host.calls.iter().any(|c| c.contains(r#""action":"fps_target","value":30"#)));
}

#[test]
fn settings_go_to_the_phone_once_the_stream_has_started() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    started(&mut p, &mut host);
    assert!(host.is_scheduled(T_INITIAL));
    p.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: T_INITIAL.into() });
    let sent: Vec<&String> = host.calls.iter().filter(|c| c.starts_with("phone_control")).collect();
    assert_eq!(sent.len(), 3);
    assert!(sent[0].contains("jpeg_quality") && sent[1].contains("bitrate") && sent[2].contains("fps_target"));
}

#[test]
fn over_usb_the_wifi_hint_says_nothing() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    p.on_stream_start(&mut host, &StreamInfo { source_id: "p".into(), url: "https://127.0.0.1:40001/v1/video.h264".into() });
    p.wifi = (Some("2.4".into()), Some(-80), None);
    assert_eq!(p.wifi_hint(), "");
}

#[test]
fn the_format_key_and_bitrate_key_work_like_the_controls() {
    let (mut p, mut host) = (plugin(), FakeHost::new());
    assert_eq!(p.run_action(&mut host, "stream.format", &json!({"op": "next"})).as_deref(), Some("Format: Heavy"));
    assert_eq!(p.stream_format(), FORMAT_MJPEG);
    assert_eq!(p.run_action(&mut host, "stream.quality", &json!({"op": "set", "value": 60})).as_deref(), Some("JPEG quality: 60%"));
    assert_eq!(p.run_action(&mut host, "stream.bitrate", &json!({"op": "up", "amount": 5})), None, "hidden while Heavy streams");
    p.run_action(&mut host, "stream.format", &json!({"op": "set", "option": "Light"}));
    assert_eq!(p.run_action(&mut host, "stream.bitrate", &json!({"op": "set", "value": 101})).as_deref(), Some("Bitrate: Dynamic"));
    assert_eq!(p.actions().len(), 6);
}

#[test]
fn diagnostics_say_what_was_chosen_and_what_runs() {
    let mut p = StreamOutputPlugin::new(false);
    p.set_config(&json!({"format": "h264"})).unwrap();
    let d: HashMap<String, String> = p.diagnostics().into_iter().collect();
    assert_eq!(d["Format"], "mjpeg (H.264 chosen)");
    assert_eq!(d["Phone codecs"], "unknown");
    assert_eq!(d["Bitrate"], "Auto");
}

#[test]
fn the_card_compiles_with_the_kit() {
    crate::ui_check::compiles(UI, "stream_output.slint", "StreamOutputCard");
}
