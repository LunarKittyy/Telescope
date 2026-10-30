use super::*;
use telescope_core::host::HostShell;
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
    p.on_stream_start(host, &StreamInfo { source_id: "phone1".into(), url: "https://192.168.1.5:8443/v1/video.h264".into(), session: 0 });
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
    p.on_stream_start(&mut host, &StreamInfo { source_id: "p".into(), url: "https://127.0.0.1:40001/v1/video.h264".into(), session: 0 });
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

// test_plugins.py: the stream_output fixture's tests, as the card and the phone see them

fn cam(id: &str, sizes: &[(u32, u32)]) -> Value {
    let list: Vec<Value> = sizes.iter().map(|(w, h)| json!({"width": w, "height": h})).collect();
    json!({"id": id, "current": true, "supportedSizes": list})
}

/// The state of a phone at (w, h) with one lens and nothing else to say.
fn on_lens(sizes: &[(u32, u32)], w: u32, h: u32) -> Value {
    json!({"cameras": [cam("0", sizes)], "stream_width": w, "stream_height": h})
}

fn pick(p: &mut StreamOutputPlugin, host: &mut FakeHost, callback: &str, arg: Value) {
    p.on_ui(host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
}

fn pick_size(p: &mut StreamOutputPlugin, host: &mut FakeHost, wh: (u32, u32)) {
    let i = p.sizes.res.iter().position(|s| *s == wh).unwrap();
    pick(p, host, "res-picked", json!(i));
}

fn pick_ratio_of(p: &mut StreamOutputPlugin, host: &mut FakeHost, ratio: (u32, u32)) {
    let i = p.sizes.ratios.iter().position(|r| *r == ratio).unwrap();
    pick(p, host, "ar-picked", json!(i));
}

fn pick_fps(p: &mut StreamOutputPlugin, host: &mut FakeHost, fps: u32) {
    let i = FPS_CHOICES.iter().position(|f| *f == fps).unwrap();
    pick(p, host, "fps-picked", json!(i));
}

fn sent(host: &FakeHost) -> Vec<Value> {
    host.calls.iter().filter_map(|c| c.strip_prefix("phone_control ")).map(|c| serde_json::from_str(c.split_once(' ').unwrap().1).unwrap()).collect()
}

fn reconnects(host: &FakeHost) -> usize {
    host.calls.iter().filter(|c| *c == "reconnect_stream").count()
}

fn saves(host: &FakeHost) -> usize {
    host.calls.iter().filter(|c| *c == "schedule_save").count()
}

fn issue<'a>(host: &'a FakeHost, key: &str) -> Option<&'a Issue> {
    host.issues.iter().find(|(k, _)| k == key).map(|(_, i)| i)
}

fn labels(i: &Issue) -> Vec<&str> {
    i.actions.iter().map(|a| a.label.as_str()).collect()
}

fn phone_a() -> (StreamOutputPlugin, FakeHost) {
    let mut host = FakeHost::new();
    host.focused = Some("phone-a".into());
    (plugin(), host)
}

fn behind(p: &mut StreamOutputPlugin, host: &mut FakeHost, sid: &str, b: bool) {
    p.on_event(host, &Event::StreamBehind { source_id: sid.into(), behind: b });
}

fn start(p: &mut StreamOutputPlugin, host: &mut FakeHost, url: &str) {
    p.on_stream_start(host, &StreamInfo { source_id: "phone-a".into(), url: url.into(), session: 1 });
}

fn diag(p: &StreamOutputPlugin) -> HashMap<String, String> {
    p.diagnostics().into_iter().collect()
}

const TWO_SIZES: [(u32, u32); 3] = [(4096, 3072), (1920, 1080), (1280, 720)];

fn aspects() -> Value {
    json!({"cameras": [{"id": "0", "current": true,
        "supportedSizes": [{"width": 1920, "height": 1440}, {"width": 1440, "height": 1080}, {"width": 640, "height": 480},
                           {"width": 1920, "height": 1080}, {"width": 1280, "height": 720}],
        "h264Sizes": [{"width": 1440, "height": 1080}, {"width": 640, "height": 480},
                      {"width": 1920, "height": 1080}, {"width": 1280, "height": 720}]}],
        "codecs": ["mjpeg", "h264"], "codec": "h264", "stream_width": 1920, "stream_height": 1080})
}

fn fps_state(max_fps: Value) -> Value {
    json!({"cameras": [{"id": "0", "current": true, "maxFps": max_fps, "supportedSizes": [{"width": 1920, "height": 1080}]}]})
}

fn stopped_after(p: &mut StreamOutputPlugin, host: &mut FakeHost, state: &Value) {
    start(p, host, "url");
    p.on_phone_state(host, state);
    p.on_stream_stop(host);
}

#[test]
fn stream_output_defaults_and_resolution_placeholder() {
    let p = plugin();
    assert_eq!(p.stream_params(), (None, None, 30));
    assert_eq!(p.props()["res-items"], json!(["—"]));
    assert_eq!(p.props()["res-enabled"], false);
}

#[test]
fn stream_output_resolution_populates_from_phone_state() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (1280, 720), (854, 480)], 1280, 720));
    assert_eq!(p.props()["res-enabled"], true);
    assert_eq!(p.sizes.current_text(), "1920 × 1080");
    assert_eq!(p.props()["res-items"], json!(["1920 × 1080", "1280 × 720", "854 × 480"]));
    assert_eq!(p.stream_params(), (None, None, 30));
}

#[test]
fn resolution_shortcuts_accept_old_and_current_labels() {
    for option in ["1280 x 720", "1280 × 720"] {
        let (mut p, mut host) = phone_a();
        start(&mut p, &mut host, "https://phone/v1/video.h264");
        p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (1280, 720)], 1920, 1080));
        assert_eq!(p.run_action(&mut host, "stream.resolution", &json!({"op": "set", "option": option})).as_deref(), Some("Resolution: 1280 × 720"));
        assert_eq!(p.sizes.current(), Some((1280, 720)));
        assert_eq!(sent(&host).last().unwrap(), &json!({"action": "resolution", "width": 1280, "height": 720}));
        let resolution = p.actions().into_iter().find(|a| a.id == "stream.resolution").unwrap();
        assert!(resolution.params[1]["choices"].as_array().unwrap().contains(&json!(["1280 x 720", "1280 × 720"])));
        p.on_stream_stop(&mut host);
    }
}

#[test]
fn stream_output_diagnostics_say_which_h264_profile_the_phone_runs() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &json!({"cameras": [], "codec": "h264", "h264_profile": "high"}));
    assert_eq!(diag(&p)["H.264 profile"], "high");
    assert_eq!(diag(&p)["H.264 refresh"], "keyframes");
    p.on_phone_state(&mut host, &json!({"cameras": [], "codec": "h264", "h264_profile": "high", "intra_refresh": true}));
    assert_eq!(diag(&p)["H.264 refresh"], "intra refresh");
    p.on_phone_state(&mut host, &json!({"cameras": []}));
    assert!(!diag(&p).contains_key("H.264 profile"));
}

#[test]
fn stream_output_saved_resolution_wins_over_default_and_live() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "1280 × 720"})).unwrap();
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (1280, 720)], 1920, 1080));
    assert_eq!(p.sizes.current_text(), "1280 × 720");
}

#[test]
fn stream_output_resolution_selection_sends_control() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (1280, 720)], 1920, 1080));
    pick_size(&mut p, &mut host, (1280, 720));
    assert!(sent(&host).contains(&json!({"action": "resolution", "width": 1280, "height": 720})));
    assert_eq!(p.stream_params(), (None, None, 30));
}

#[test]
fn stream_output_camera_switch_carries_over_the_current_resolution() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &on_lens(&[(4032, 3024), (1920, 1080)], 1920, 1080));
    pick_ratio_of(&mut p, &mut host, (16, 9));
    pick_size(&mut p, &mut host, (1920, 1080));
    p.on_event(&mut host, &Event::CameraSwitched { camera: cam("1", &[(4032, 3024), (1920, 1080)]) });
    assert_eq!(p.sizes.current_text(), "1920 × 1080");
}

#[test]
fn stream_output_camera_switch_falls_back_when_new_lens_lacks_the_size() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &on_lens(&[(4032, 3024), (640, 480)], 640, 480));
    pick_size(&mut p, &mut host, (640, 480));
    p.on_event(&mut host, &Event::CameraSwitched { camera: cam("1", &[(4032, 3024), (1920, 1080)]) });
    assert_eq!(p.sizes.current_text(), "4032 × 3024");
}

#[test]
fn a_lens_without_the_live_size_sends_the_size_it_falls_back_to() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080)], 1920, 1080));
    host.calls.clear();
    p.on_event(&mut host, &Event::CameraSwitched { camera: cam("1", &[(1280, 720), (1024, 768)]) });
    let (w, h) = p.sizes.current().unwrap();
    assert!(sent(&host).contains(&json!({"action": "resolution", "width": w, "height": h})));
}

#[test]
fn a_size_can_be_picked_while_stopped_and_the_next_start_opens_at_it() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "4096 × 3072", "fps": 60})).unwrap();
    stopped_after(&mut p, &mut host, &on_lens(&TWO_SIZES, 4096, 3072));
    assert_eq!(p.props()["res-enabled"], true);
    assert_eq!(p.sizes.current_text(), "4096 × 3072");
    assert_eq!(p.opening(), json!({"width": 4096, "height": 3072, "fps": 60}));

    host.calls.clear();
    pick_ratio_of(&mut p, &mut host, (16, 9));
    pick_size(&mut p, &mut host, (1280, 720));
    assert!(sent(&host).is_empty(), "nothing to send it to");
    assert_eq!(p.opening(), json!({"width": 1280, "height": 720, "fps": 60}));
    assert_eq!(p.get_config()["resolution"], "1280 x 720");
    assert!(saves(&host) >= 1);

    host.calls.clear();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &on_lens(&TWO_SIZES, 1280, 720));
    assert_eq!(p.sizes.current_text(), "1280 × 720");
    assert!(!sent(&host).iter().any(|m| m["action"] == "resolution"));
}

#[test]
fn a_new_aspect_ratio_keeps_the_height_rather_than_jumping_to_the_largest() {
    let mut p = StreamOutputPlugin::new(false); // Heavy: every size listed
    let mut host = FakeHost::new();
    p.set_config(&json!({"resolution": "1920 × 1080"})).unwrap();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &aspects());
    assert_eq!(p.sizes.current_text(), "1920 × 1080");
    pick_ratio_of(&mut p, &mut host, (4, 3));
    assert_eq!(p.sizes.current_text(), "1440 × 1080");
    assert_eq!(sent(&host).last().unwrap(), &json!({"action": "resolution", "width": 1440, "height": 1080}));
}

#[test]
fn light_lists_only_the_sizes_the_phone_s_encoder_takes() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "1920 × 1440"})).unwrap();
    assert_eq!(p.opening()["width"], 1920);
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &aspects());
    pick_ratio_of(&mut p, &mut host, (4, 3));
    assert_eq!(p.props()["res-items"], json!(["1440 × 1080", "640 × 480"]));
    p.on_stream_stop(&mut host);
    p.set_config(&json!({"resolution": "1920 × 1440"})).unwrap();
    assert_eq!(p.opening(), json!({"fps": 30, "width": 1440, "height": 1080}));
}

#[test]
fn a_size_too_much_for_the_encoder_goes_back_to_the_last_that_worked() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "1920 × 1080"})).unwrap();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &aspects());
    pick_size(&mut p, &mut host, (1280, 720));
    let mut failed = aspects();
    failed["codec"] = json!("mjpeg");
    failed["stream_width"] = json!(1280);
    failed["stream_height"] = json!(720);
    failed["codec_error"] = json!("The camera can't feed H.264 at 1280x720");
    failed["codec_unsupported"] = json!(true);
    p.on_phone_state(&mut host, &failed);
    let i = issue(&host, "encoder").unwrap();
    assert!(i.text.contains("set back to 1920 × 1080"), "{}", i.text);
    assert_eq!(labels(i), ["Start at 1920 × 1080", "Switch to Heavy"]);
    p.on_stream_stop(&mut host);
    assert_eq!(p.get_config()["resolution"], "1920 x 1080");
    assert_eq!(p.opening()["width"], 1920);
}

#[test]
fn without_a_known_size_the_start_leaves_it_to_the_phone() {
    let mut p = plugin();
    p.set_config(&json!({"fps": 30})).unwrap();
    assert_eq!(p.opening(), json!({"fps": 30}));
    p.set_config(&json!({"resolution": "1920 × 1080", "fps": 30})).unwrap();
    assert_eq!(p.opening(), json!({"width": 1920, "height": 1080, "fps": 30}));
}

#[test]
fn another_phone_does_not_keep_this_one_s_sizes() {
    let (mut p, mut host) = phone_a();
    stopped_after(&mut p, &mut host, &on_lens(&TWO_SIZES, 4096, 3072));
    p.on_event(&mut host, &Event::DeviceChanged { source_id: "other phone".into() });
    assert_eq!(p.sizes.current_text(), "—");
    assert_eq!(p.props()["res-enabled"], false);
}

#[test]
fn a_preset_applied_while_stopped_is_what_the_next_start_opens_at() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "4096 × 3072"})).unwrap();
    stopped_after(&mut p, &mut host, &on_lens(&TWO_SIZES, 4096, 3072));
    p.apply_preset(&mut host, &json!({"resolution": "1920 × 1080", "fps": 30})).unwrap();
    assert_eq!(p.sizes.current_text(), "1920 × 1080");
    assert_eq!(p.opening(), json!({"width": 1920, "height": 1080, "fps": 30}));
    assert_eq!(p.get_config()["resolution"], "1920 x 1080");
}

#[test]
fn stream_output_fps_hot_updates_worker_and_phone_together() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    pick_fps(&mut p, &mut host, 48);
    assert!(host.calls.iter().any(|c| c.starts_with("update_stream_output") && c.contains("fps: Some(Some(48))")), "{:?}", host.calls);
    assert!(sent(&host).contains(&json!({"action": "fps_target", "value": 48})));
    assert!(saves(&host) >= 1);
}

#[test]
fn rates_past_the_lens_are_grayed_and_the_pick_comes_back_on_a_lens_that_can() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    pick_fps(&mut p, &mut host, 60);
    host.calls.clear();
    p.on_phone_state(&mut host, &fps_state(json!(30)));
    assert_eq!(p.props()["fps-gone"], json!([false, false, false, false, true, true]));
    assert_eq!(p.fps(), 30);
    assert!(sent(&host).contains(&json!({"action": "fps_target", "value": 30})));
    assert!(host.calls.iter().rev().any(|c| c.contains("fps: Some(Some(30))")));
    assert_eq!(p.get_config()["fps"], 60);
    p.on_phone_state(&mut host, &fps_state(json!(60)));
    assert_eq!(p.fps(), 60);
}

#[test]
fn a_phone_that_doesnt_say_leaves_every_rate_open() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &fps_state(Value::Null));
    assert_eq!(p.props()["fps-gone"], json!([false, false, false, false, false, false]));
}

#[test]
fn a_saved_rate_from_before_the_list_lands_on_the_nearest_choice() {
    for (saved, shown) in [(json!(45), 48), (json!(5), 15), (json!(27), 25), (json!(60), 60), (json!("junk"), 30)] {
        let mut p = plugin();
        p.set_config(&json!({"fps": saved})).unwrap();
        assert_eq!(p.fps(), shown, "{saved}");
    }
}

#[test]
fn stream_output_phone_settings_lifecycle() {
    let (mut p, mut host) = phone_a();
    pick(&mut p, &mut host, "quality-moved", json!(92));
    pick_fps(&mut p, &mut host, 25);
    start(&mut p, &mut host, "url");
    p.push_initial_settings(&mut host);
    pick(&mut p, &mut host, "quality-moved", json!(91));
    let got = sent(&host);
    assert!(got.contains(&json!({"action": "jpeg_quality", "value": 92})));
    assert!(got.contains(&json!({"action": "fps_target", "value": 25})));
    assert!(got.contains(&json!({"action": "jpeg_quality", "value": 91})));
    assert_eq!(p.props()["quality-readout"], "91%");
    assert!(p.props()["quality-tip"].as_str().unwrap().contains("High"));

    p.on_stream_stop(&mut host);
    let before = sent(&host).len();
    p.push_initial_settings(&mut host);
    assert_eq!(sent(&host).len(), before);
}

#[test]
fn stream_output_config_round_trip_and_invalid_resolution() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "854 x 480", "fps": 48, "jpeg_quality": 77})).unwrap();
    assert_eq!(p.get_config(), json!({"fps": 48, "jpeg_quality": 77, "format": "h264", "bitrate_mbps": 0, "resolution": "854 x 480"}));
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (854, 480)], 1920, 1080));
    assert_eq!(p.sizes.current_text(), "854 × 480");
    assert_eq!(p.get_config()["resolution"], "854 x 480");
}

#[test]
fn stream_output_invalid_persisted_resolution_falls_back_to_first() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"resolution": "not a real size"})).unwrap();
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (854, 480)], 999, 999));
    assert_eq!(p.sizes.current_text(), "1920 × 1080");
}

#[test]
fn stream_output_keeps_the_saved_resolution_through_an_idle_save() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &on_lens(&[(1920, 1080), (1280, 720)], 1920, 1080));
    pick_size(&mut p, &mut host, (1280, 720));
    p.on_stream_stop(&mut host);
    assert_eq!(p.get_config()["resolution"], "1280 x 720");
}

#[test]
fn stream_output_saved_resolution_does_not_leak_into_the_next_device() {
    let mut p = plugin();
    p.set_config(&json!({"resolution": "1280 × 720"})).unwrap();
    p.set_config(&json!({"fps": 30, "jpeg_quality": 85})).unwrap();
    assert!(p.get_config().get("resolution").is_none());
    assert_eq!(p.pending_resolution_text, None);
}

#[test]
fn light_is_the_default_and_switching_reconnects() {
    let (mut p, mut host) = phone_a();
    assert_eq!(p.stream_format(), "h264");
    assert_eq!(p.props()["h264"], true);
    assert_eq!(p.props()["h264-enabled"], true);
    assert!(p.props()["light-tip"].as_str().unwrap().contains("H.264"));
    assert!(p.props()["heavy-tip"].as_str().unwrap().contains("MJPEG"));

    p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"]}));
    assert_eq!(p.stream_format(), "h264");
    assert_eq!(reconnects(&host), 0);

    pick(&mut p, &mut host, "format-picked", json!(false));
    assert_eq!(p.stream_format(), "mjpeg");
    assert_eq!(reconnects(&host), 1);
    assert_eq!(p.get_config()["format"], "mjpeg");
    assert_eq!(p.props()["h264"], false);

    pick(&mut p, &mut host, "format-picked", json!(true));
    assert_eq!(p.stream_format(), "h264");
    assert_eq!(reconnects(&host), 2);
}

#[test]
fn a_phone_without_an_encoder_quietly_gets_heavy() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &json!({}));
    assert_eq!(p.stream_format(), "h264");
    assert_eq!(reconnects(&host), 0);
    p.on_phone_state(&mut host, &json!({"cameras": []}));
    assert_eq!(p.stream_format(), "mjpeg");
    assert_eq!(reconnects(&host), 1);
    assert_eq!(p.get_config()["format"], "mjpeg");
    assert_eq!(p.props()["h264-enabled"], false);
    assert_eq!(p.props()["h264"], false);
    assert!(p.props()["light-tip"].as_str().unwrap().contains("no H.264 encoder"));
    assert!(host.issues.is_empty());
}

#[test]
fn switching_away_from_a_phone_without_an_encoder_keeps_the_next_ones_light() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &json!({"cameras": []}));
    assert_eq!(p.stream_format(), "mjpeg");
    p.set_config(&json!({"format": "h264"})).unwrap();
    p.on_event(&mut host, &Event::DeviceChanged { source_id: "second".into() });
    assert_eq!(p.stream_format(), "h264");
    assert_eq!(p.get_config()["format"], "h264");
}

#[test]
fn h264_needs_a_decoder_here() {
    let mut p = StreamOutputPlugin::new(false);
    let mut host = FakeHost::new();
    p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"]}));
    assert_eq!(p.props()["h264-enabled"], false);
    assert!(p.props()["light-tip"].as_str().unwrap().contains("PyAV"));
    assert_eq!(p.stream_format(), "mjpeg");
    assert_eq!(p.props()["h264"], false);
}

#[test]
fn an_encoder_failure_goes_back_to_mjpeg_with_a_banner() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"format": "h264"})).unwrap();
    p.on_phone_state(&mut host, &json!({"codecs": ["mjpeg", "h264"], "codec": "mjpeg", "codec_error": "The phone's H.264 encoder stopped"}));
    assert_eq!(p.stream_format(), "mjpeg");
    assert_eq!(reconnects(&host), 1);
    assert!(issue(&host, "h264").unwrap().text.contains("encoder stopped"));
}

#[test]
fn falling_behind_on_heavy_suggests_light_with_a_button() {
    let (mut p, mut host) = phone_a();
    p.set_config(&json!({"format": "mjpeg"})).unwrap();
    host.show_issue("h264", Issue { title: "Switched to Heavy".into(), text: String::new(), actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: false });
    behind(&mut p, &mut host, "phone-a", true);
    let i = issue(&host, "behind").unwrap();
    assert_eq!((i.title.as_str(), i.text.as_str(), i.kind.as_str()), ("Can't keep up", "Try Light or lower quality.", "warn"));
    assert_eq!(labels(i), ["Switch to Light"]);
    let IssueDoes::Plugin { id } = &i.actions[0].does else { panic!("the plugin's own button") };
    let id = id.clone();

    p.on_issue_action(&mut host, "behind", &id);
    assert_eq!(p.stream_format(), "h264");
    assert_eq!(p.get_config()["format"], "h264");
    assert_eq!(p.props()["h264"], true);
    assert_eq!(reconnects(&host), 1);
    assert!(issue(&host, "h264").is_none());

    behind(&mut p, &mut host, "phone-a", false);
    assert!(issue(&host, "behind").is_none());
}

#[test]
fn falling_behind_without_light_to_offer_has_no_button() {
    for (fmt, decodable, codecs) in [("h264", true, json!(["mjpeg", "h264"])), ("mjpeg", false, json!(["mjpeg", "h264"])), ("mjpeg", true, json!(["mjpeg"]))] {
        let mut p = StreamOutputPlugin::new(decodable);
        let mut host = FakeHost::new();
        host.focused = Some("phone-a".into());
        p.set_config(&json!({"format": fmt})).unwrap();
        p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": codecs}));
        let before = reconnects(&host);
        behind(&mut p, &mut host, "phone-a", true);
        let i = issue(&host, "behind").unwrap();
        assert_eq!(i.text, "Try lower quality.");
        assert!(i.actions.is_empty());
        assert_eq!(reconnects(&host), before);
    }
}

#[test]
fn falling_behind_on_light_offers_dynamic() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"], "dynamic_bitrate": true}));
    behind(&mut p, &mut host, "phone-a", true);
    let i = issue(&host, "behind").unwrap();
    assert_eq!(i.text, "Try Dynamic or lower quality.");
    assert_eq!(labels(i), ["Switch to Dynamic"]);

    p.on_issue_action(&mut host, "behind", A_DYNAMIC);
    assert_eq!(sent(&host).last().unwrap(), &json!({"action": "bitrate", "value": -1}));
    assert_eq!(p.props()["bitrate-readout"], "Dynamic");
    assert_eq!(p.get_config()["bitrate_mbps"], -1);
    assert_eq!(reconnects(&host), 0);

    behind(&mut p, &mut host, "phone-a", false);
    behind(&mut p, &mut host, "phone-a", true);
    let i = issue(&host, "behind").unwrap();
    assert_eq!(i.text, "Try a lower resolution or FPS.");
    assert!(i.actions.is_empty());
}

#[test]
fn dynamic_is_the_bitrate_slider_all_the_way_right() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    pick(&mut p, &mut host, "bitrate-moved", json!(101));
    assert_eq!(sent(&host).last().unwrap(), &json!({"action": "bitrate", "value": -1}));
    assert_eq!(p.props()["bitrate-readout"], "Dynamic");
    pick(&mut p, &mut host, "bitrate-moved", json!(30));
    assert_eq!(sent(&host).last().unwrap(), &json!({"action": "bitrate", "value": 30_000_000}));
    assert_eq!(p.props()["bitrate-readout"], "30 Mbps");
    pick(&mut p, &mut host, "bitrate-moved", json!(101));
    assert_eq!(p.get_config()["bitrate_mbps"], -1);
    p.set_config(&json!({"bitrate_mbps": 0})).unwrap();
    assert_eq!(p.props()["bitrate-readout"], "Auto");
    p.set_config(&json!({"bitrate_mbps": -1})).unwrap();
    assert_eq!(p.props()["bitrate-readout"], "Dynamic");
    assert_eq!(p.bitrate_bps(), -1);
    p.set_config(&json!({"bitrate_mbps": 999})).unwrap();
    assert_eq!(p.props()["bitrate-readout"], "100 Mbps");
    p.push_initial_settings(&mut host);
    assert!(sent(&host).contains(&json!({"action": "bitrate", "value": 100_000_000})));
}

#[test]
fn a_phone_too_old_for_dynamic_says_so() {
    let (mut p, mut host) = phone_a();
    let tip = |p: &StreamOutputPlugin| p.props()["bitrate-tip"].as_str().unwrap().to_string();
    p.set_config(&json!({"bitrate_mbps": -1})).unwrap();
    assert!(!tip(&p).contains("too old"));
    p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"]}));
    assert!(tip(&p).contains("too old for Dynamic"));
    assert_eq!(diag(&p)["Bitrate"], "Dynamic (phone uses Auto)");
    p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"], "dynamic_bitrate": true}));
    assert!(!tip(&p).contains("too old"));
    assert_eq!(diag(&p)["Bitrate"], "Dynamic");
    p.on_event(&mut host, &Event::DeviceChanged { source_id: "another".into() });
    assert!(!tip(&p).contains("too old"));
}

#[test]
fn dynamic_is_no_wider_than_the_readouts_already_there() {
    use telescope_core::text::{app_font, Face, Weight};
    let face = Face::em(app_font(Weight::SemiBold).expect("a font"), 13.0);
    assert!(face.advance("Dynamic") <= face.advance("100 Mbps"));
    assert!(face.advance("100 Mbps") + 2.0 <= 64.0, "the card's readout column is 64px");
}

#[test]
fn h264_too_much_for_the_encoder_stops_and_offers_heavy() {
    let (mut p, mut host) = phone_a();
    host.focused = Some("phone-b".into());
    p.on_phone_state(&mut host, &json!({"cameras": [], "codecs": ["mjpeg", "h264"], "codec": "mjpeg",
        "codec_error": "H.264 isn't available at 4096x3072 on this phone", "codec_unsupported": true}));
    p.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: T_STOP.into() });
    assert_eq!(host.calls.iter().filter(|c| c.starts_with("stop_stream")).count(), 1);
    assert!(host.calls.contains(&r#"stop_stream Some("phone-b")"#.to_string()));
    assert_eq!(reconnects(&host), 0);
    assert_eq!(p.stream_format(), "h264", "switching is the user's call");
    let i = issue(&host, "encoder").unwrap();
    assert_eq!(i.title, "Too much for the phone's H.264 encoder");
    assert!(i.text.starts_with("H.264 isn't available at 4096x3072") && i.text.contains("lower resolution or FPS"), "{}", i.text);
    assert_eq!(labels(i), ["Switch to Heavy"]);
    let IssueDoes::StartAgain { source_id, before } = &i.actions[0].does else { panic!("starts again") };
    assert_eq!(source_id.as_deref(), Some("phone-b"), "the phone it was about, even with focus moved on");
    let before = before.clone().unwrap();
    p.on_issue_action(&mut host, "encoder", &before);
    assert_eq!(p.stream_format(), "mjpeg");
    assert_eq!(p.get_config()["format"], "mjpeg");
}

#[test]
fn bitrate_goes_up_to_100_mbps() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    pick(&mut p, &mut host, "bitrate-moved", json!(100));
    assert_eq!(sent(&host).last().unwrap(), &json!({"action": "bitrate", "value": 100_000_000}));
    assert_eq!(p.props()["bitrate-readout"], "100 Mbps");
    assert_eq!(DYNAMIC_POS, 101);
    assert!(UI.contains("maximum: 101;"));
    p.set_config(&json!({"bitrate_mbps": 80})).unwrap();
    assert_eq!(p.props()["bitrate-readout"], "80 Mbps");
}

#[test]
fn bitrate_is_sent_in_bits_per_second() {
    let (mut p, mut host) = phone_a();
    start(&mut p, &mut host, "url");
    pick(&mut p, &mut host, "bitrate-moved", json!(12));
    assert_eq!(sent(&host).last().unwrap(), &json!({"action": "bitrate", "value": 12_000_000}));
    assert_eq!(p.props()["bitrate-readout"], "12 Mbps");
    pick(&mut p, &mut host, "bitrate-moved", json!(0));
    assert_eq!(p.props()["bitrate-readout"], "Auto");
}

#[test]
fn jpeg_quality_stops_at_95_and_marks_the_recommended_spot() {
    let mut p = plugin();
    assert_eq!(MAX_QUALITY, 95);
    assert!(UI.contains("maximum: 95;") && UI.contains("marks: [85];"));
    assert!(p.props()["quality-tip"].as_str().unwrap().contains("recommended"));
    p.set_config(&json!({"jpeg_quality": 100})).unwrap();
    assert_eq!(p.get_config()["jpeg_quality"], 95);
}

#[test]
fn falling_behind_leaves_another_source_to_say_what_helps() {
    let (mut p, mut host) = phone_a();
    p.on_event(&mut host, &Event::SourceSelected { source_id: "browser".into() });
    behind(&mut p, &mut host, "phone-a", true);
    assert!(issue(&host, "behind").is_none());
    p.on_event(&mut host, &Event::SourceSelected { source_id: String::new() });
    behind(&mut p, &mut host, "phone-a", true);
    assert!(issue(&host, "behind").is_some());
}

#[test]
fn another_stream_falling_behind_says_nothing_but_catching_up_clears_the_note() {
    let (mut p, mut host) = phone_a();
    behind(&mut p, &mut host, "phone-b", true);
    assert!(issue(&host, "behind").is_none());
    behind(&mut p, &mut host, "phone-a", true);
    behind(&mut p, &mut host, "phone-b", false);
    assert!(issue(&host, "behind").is_none());
}

#[test]
fn falling_behind_says_when_the_phones_wifi_is_why() {
    let wifi = "https://10.0.0.5:8080/v1/video.h264";
    for (extra, url, starts) in [
        (json!({"wifi_band": "5", "wifi_rssi": -78}), wifi, "The phone's Wi-Fi signal is weak"),
        (json!({"wifi_band": "2.4", "wifi_rssi": -50}), wifi, "The phone is on 2.4 GHz Wi-Fi"),
        (json!({"wifi_band": "5", "wifi_rssi": -50}), wifi, "Try "),
        (json!({"wifi_band": "2.4", "wifi_rssi": -80}), "https://127.0.0.1:41000/v1/video.h264", "Try "),
        (json!({}), wifi, "Try "),
    ] {
        let (mut p, mut host) = phone_a();
        start(&mut p, &mut host, url);
        let mut s = json!({"cameras": [], "codecs": ["mjpeg", "h264"], "dynamic_bitrate": true});
        s.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        p.on_phone_state(&mut host, &s);
        behind(&mut p, &mut host, "phone-a", true);
        let text = &issue(&host, "behind").unwrap().text;
        assert!(text.starts_with(starts) && text.ends_with("Try Dynamic or lower quality."), "{text}");
    }
}

#[test]
fn diagnostics_show_the_phones_wifi() {
    let (mut p, mut host) = phone_a();
    p.on_phone_state(&mut host, &json!({"cameras": [], "wifi_band": "5", "wifi_rssi": -58, "wifi_mbps": 866}));
    assert_eq!(diag(&p)["Phone Wi-Fi"], "5 GHz, -58 dBm, 866 Mbps link");
    p.on_phone_state(&mut host, &json!({"cameras": []}));
    assert!(!diag(&p).contains_key("Phone Wi-Fi"));
}
