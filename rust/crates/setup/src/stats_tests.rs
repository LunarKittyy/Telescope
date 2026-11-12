use super::*;
use telescope_core::balance::{Balance, Place};
use std::sync::Arc;
use telescope_core::stats::{DropReason, Hub};
use telescope_core::host::HostUi;
use telescope_core::testing::FakeHost;
use crate::stats_phone::PhoneReading;

const SECOND: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;

/// A stream that dropped known frames and was slow in a known place: the snapshot a second in.
fn snapshot(hub: &Hub, source: &str) -> Snapshot {
    let stats = hub.register(source, 0);
    stats.set_link("https://127.0.0.1:40001/v1/video.h264", false);
    stats.set_target_fps(30);
    stats.received(24, 250_000);
    stats.decoded(23);
    for _ in 0..22 {
        stats.delivered();
    }
    for _ in 0..4 {
        stats.repeated();
    }
    stats.dropped(DropReason::LostOnLink, 3);
    stats.dropped(DropReason::Paced, 1);
    hub.snapshot_at(telescope_core::time::monotonic_ns() + 10 * SECOND, Some(SECOND), Some(200 << 20))
}

fn props(host: &FakeHost) -> Value {
    let call = host.calls.iter().rev().find(|c| c.starts_with("set_ui_props card ")).expect("the card was set");
    serde_json::from_str(call.trim_start_matches("set_ui_props card ")).unwrap()
}

fn row(props: &Value, label: &str) -> Value {
    props["rows"].as_array().unwrap().iter().find(|r| r["label"] == label).unwrap_or_else(|| panic!("no row {label:?} in {}", props["rows"])).clone()
}

fn has_row(props: &Value, label: &str) -> bool {
    props["rows"].as_array().unwrap().iter().any(|r| r["label"] == label)
}

fn set_up(snap: Snapshot) -> (StatsPlugin, FakeHost) {
    let mut host = FakeHost::new();
    host.focused = Some("phone".into());
    let mut plugin = StatsPlugin::new();
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snap });
    (plugin, host)
}

#[test]
fn it_is_an_optional_card_among_the_streaming_ones() {
    let info = StatsPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.optional, info.required), ("stats", "streaming", true, false));
    assert_eq!(StatsPlugin::new().card().unwrap().region, "right");
}

#[test]
fn the_cards_numbers_are_the_streams() {
    let hub = Hub::new();
    let (_plugin, host) = set_up(snapshot(&hub, "phone"));
    let p = props(&host);
    assert_eq!((row(&p, "Frames in")["value"].as_str(), row(&p, "Frames in")["note"].as_str()), (Some("2.4 fps"), Some("asked for 30.0 fps")));
    assert_eq!(row(&p, "Frames out")["value"], "2.2 fps");
    assert_eq!((row(&p, "Frames out")["note"].as_str(), row(&p, "Repeated")["value"].as_str()), (Some("22 in all"), Some("0.4 fps")));
    assert_eq!(row(&p, "Dropped")["note"], "4 in all");
    assert_eq!(row(&p, "Lost on the link")["value"], "3");
    assert_eq!(row(&p, "Dropped by pacing")["value"], "1");
    assert!(!has_row(&p, "Arrived too late"), "only the reasons that happened");
    assert_eq!((row(&p, "Connection")["value"].as_str(), row(&p, "Connection")["note"].as_str()), (Some("USB"), Some("TCP")));
    assert_eq!(p["empty-text"], "");
    assert_eq!(p["view"], 0);
    // Standard leaves the Nerd rows out
    assert!(!has_row(&p, "Decoded") && !has_row(&p, "Frame steps run on"));
    // The app's own numbers are there in both views, since they can be pinned
    assert!(has_row(&p, "Memory"));
}

#[test]
fn no_drops_says_none() {
    let hub = Hub::new();
    let stats = hub.register("phone", 0);
    stats.received(30, 1000);
    let (_plugin, host) = set_up(hub.snapshot_at(telescope_core::time::monotonic_ns() + SECOND, None, None));
    let p = props(&host);
    assert_eq!((row(&p, "Dropped")["value"].as_str(), row(&p, "Dropped")["kind"].as_str()), (Some("None"), Some("status_ok")));
}

#[test]
fn what_the_phone_has_not_said_has_no_row_and_no_heading() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    let p = props(&host);
    assert!(!has_row(&p, "From the phone") && !has_row(&p, "Phone"), "nothing to say, so no line and no heading");
    assert!(p["rows"].as_array().unwrap().iter().all(|r| r["value"] != "Not available yet"), "no line says it isn't available");
    assert!(!has_row(&p, "Processor"), "the processor reading is not there yet either");
    assert!(has_row(&p, "Memory") && has_row(&p, "The app"), "what is measured stays");
    assert_eq!(row(&p, "Reading the virtual camera")["note"], "", "no explaining line under the apps either");
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-picked".into(), args: vec![json!(1)] });
    assert!(!has_row(&props(&host), "From the phone"));
}

#[test]
fn a_source_on_this_computer_has_no_phone_section() {
    let hub = Hub::new();
    let stats = hub.register("webcam", 0);
    stats.set_link("webcam://0", true);
    stats.set_target_fps(30);
    stats.received(30, 1000);
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + SECOND, None, None);
    for view in [View::Standard, View::Nerd] {
        let rows = rows(&snap, &snap.sources[0], view, &Context::default());
        assert!(!rows.iter().any(|r| r.head && r.label == "Phone") && !rows.iter().any(|r| r.label == "From the phone"), "{view:?}");
        assert!(rows.iter().any(|r| r.label == "Reading the virtual camera"), "the camera's readers are still shown");
    }
}

#[test]
fn the_plugin_rows_keep_their_places_whatever_the_timings_do() {
    let order = |times: [u64; 3]| {
        let hub = Hub::new();
        let now = telescope_core::time::monotonic_ns();
        hub.snapshot_at(now, Some(0), Some(0));
        hub.register("phone", 0).set_target_fps(30);
        for (name, t) in ["zeta", "alpha", "mid_one"].into_iter().zip(times) {
            hub.hooks().timer(name).add(t * MS);
        }
        let snap = hub.snapshot_at(now + SECOND, Some(SECOND), Some(0));
        let rows = rows(&snap, &snap.sources[0], View::Nerd, &Context::default());
        let at = rows.iter().position(|r| r.head && r.label == "Each plugin's events and card updates").unwrap();
        rows[at + 1..].iter().take_while(|r| !r.head).map(|r| r.label.clone()).collect::<Vec<_>>()
    };
    let first = order([9, 2, 5]);
    assert_eq!(first, ["Alpha", "Mid one", "Zeta"]);
    assert_eq!(order([1, 8, 3]), first);
    assert_eq!(order([4, 4, 40]), first);
}

#[test]
fn the_bitrate_is_shown_against_what_was_asked_for() {
    let hub = Hub::new();
    let snap = snapshot(&hub, "phone");
    let source = &snap.sources[0];
    let rows_with = |asked| rows(&snap, source, View::Standard, &Context { asked_mbps: asked, ..Context::default() });
    let note = |rows: &[Row]| rows.iter().find(|r| r.label == "Bitrate").unwrap().note.clone();
    assert_eq!(note(&rows_with(Some(8.0))), "asked for 8.0 Mbps");
    assert_eq!(note(&rows_with(Some(0.0))), "asked for Automatic");
    assert_eq!(note(&rows_with(Some(-1.0))), "asked for Dynamic");
    assert_eq!(note(&rows_with(None)), "");
}

#[test]
fn the_card_asks_for_the_stream_output_cards_bitrate_and_whether_an_app_reads_the_camera() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.plugin_configs.insert("stream_output".into(), json!({"bitrate_mbps": 12}));
    plugin.on_event(&mut host, &Event::CameraWatched { watched: true });
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snapshot(&hub, "phone") });
    let p = props(&host);
    assert_eq!(row(&p, "Bitrate")["note"], "asked for 12.0 Mbps");
    assert_eq!(row(&p, "Reading the virtual camera")["value"], "In use");
}

#[test]
fn an_extra_camera_doesnt_show_the_first_cameras_readers() {
    let hub = Hub::new();
    hub.register("second", 1).set_target_fps(30);
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + SECOND, None, None);
    let ctx = Context { watched: Some(true), ..Context::default() };
    let value = |snap: &Snapshot| rows(snap, &snap.sources[0], View::Standard, &ctx).iter().find(|r| r.label == "Reading the virtual camera").unwrap().value.clone();
    assert_eq!(value(&snap), "Not known");
    let hub = Hub::new();
    hub.register("first", 0).set_target_fps(30);
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + SECOND, None, None);
    assert_eq!(value(&snap), "In use");
}

#[test]
fn nerd_adds_times_balancer_moves_plugins_and_the_apps_cost() {
    let balance = Arc::new(Balance::free());
    for _ in 0..6 {
        balance.note_wait(25 * MS);
    }
    assert_eq!(balance.place(), Place::Cpu);
    let hub = Hub::with_balance(balance);
    hub.want("stats", true);
    let now = telescope_core::time::monotonic_ns();
    // The app's cost is a share of the time between two looks, so there has to be an earlier one
    hub.snapshot_at(now, Some(0), Some(0));
    let stats = hub.register("phone", 0);
    stats.set_target_fps(30);
    stats.steps().timer("privacy_blur").add(9 * MS);
    stats.steps().timer("colour_grading").add(2 * MS);
    hub.hooks().timer("lua_thing").add(3 * MS);
    let snap = hub.snapshot_at(now + SECOND, Some(SECOND), Some(512 << 20));
    let (mut plugin, mut host) = set_up(snap);
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-picked".into(), args: vec![json!(1)] });
    let p = props(&host);
    assert_eq!(p["view"], 1);
    assert!(has_row(&p, "Decoded"));
    assert_eq!(row(&p, "Frame steps run on")["value"], "The processor");
    assert_eq!(row(&p, "Balancer moves")["value"], "1");
    let moved = row(&p, "Moved to the processor");
    assert!(moved["note"].as_str().unwrap().contains("frames waited 25 ms for the card"), "{moved}");
    // The plugins keep to the shown names' order, not the timings'
    let labels: Vec<&str> = p["rows"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap()).collect();
    let blur = labels.iter().position(|l| *l == "Privacy blur").unwrap();
    assert!(blur > labels.iter().position(|l| *l == "Colour grading").unwrap());
    assert_eq!((row(&p, "Privacy blur")["value"].as_str(), row(&p, "Privacy blur")["note"].as_str()), (Some("9.0 ms"), Some("slowest 9.0 ms")));
    assert_eq!(row(&p, "Lua thing")["value"], "3.0 ms");
    assert_eq!(row(&p, "Processor")["value"], "100% of one core");
    assert_eq!(row(&p, "Memory")["value"], "512 MB");
}

#[test]
fn the_costly_numbers_are_only_asked_for_while_nerd_is_showing() {
    let mut host = FakeHost::new();
    let mut plugin = StatsPlugin::new();
    let wants = |host: &FakeHost| host.calls.iter().filter(|c| c.starts_with("want_stats")).cloned().collect::<Vec<_>>();
    plugin.on_plugins_ready(&mut host);
    assert!(wants(&host).is_empty(), "Standard asks for nothing");
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-picked".into(), args: vec![json!(1)] });
    assert_eq!(wants(&host), ["want_stats stats true"]);
    plugin.on_event(&mut host, &Event::WindowShown { shown: false });
    assert_eq!(wants(&host), ["want_stats stats true", "want_stats stats false"], "a window in the tray measures nothing");
    plugin.on_event(&mut host, &Event::WindowShown { shown: true });
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-picked".into(), args: vec![json!(0)] });
    assert_eq!(wants(&host).len(), 4);
    assert_eq!(wants(&host)[3], "want_stats stats false");
    plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-picked".into(), args: vec![json!(1)] });
    plugin.shutdown(&mut host);
    assert_eq!(wants(&host).last().map(String::as_str), Some("want_stats stats false"));
    assert!(!plugin.wanting());
}

#[test]
fn a_pinned_number_keeps_the_costly_measuring_going_while_the_card_is_folded() {
    let mut host = FakeHost::new();
    let mut plugin = StatsPlugin::new();
    let wants = |host: &FakeHost| host.calls.iter().filter(|c| c.starts_with("want_stats")).cloned().collect::<Vec<_>>();
    plugin.on_plugins_ready(&mut host);
    assert!(wants(&host).is_empty());
    host.pin_to_bar("stats.step_zoom", true);
    for event in std::mem::take(&mut host.events) {
        plugin.on_event(&mut host, &event);
    }
    assert_eq!(wants(&host), ["want_stats stats true"], "a pin in Standard view still needs the numbers");
    plugin.on_event(&mut host, &Event::CardFolded { key: FOLD_KEY.into(), folded: true });
    assert!(plugin.wanting(), "folding the card leaves the bar's readout measured");
    plugin.on_event(&mut host, &Event::WindowShown { shown: false });
    assert!(!plugin.wanting(), "a window in the tray measures nothing");
    plugin.on_event(&mut host, &Event::WindowShown { shown: true });
    host.pin_to_bar("stats.step_zoom", false);
    for event in std::mem::take(&mut host.events) {
        plugin.on_event(&mut host, &event);
    }
    assert!(!plugin.wanting(), "unpinning lets it go");
}

#[test]
fn a_folded_card_builds_nothing_until_it_opens_and_unchanged_numbers_are_not_resent() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    let sets = |host: &FakeHost| host.calls.iter().filter(|c| c.starts_with("set_ui_props card ")).count();
    assert_eq!(sets(&host), 1);
    let snap = plugin.latest.clone().unwrap();
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snap.clone() });
    assert_eq!(sets(&host), 1, "the same numbers aren't handed over twice");
    plugin.on_event(&mut host, &Event::CardFolded { key: FOLD_KEY.into(), folded: true });
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snap });
    assert_eq!(sets(&host), 1, "a folded card isn't fed");
    plugin.on_event(&mut host, &Event::CardFolded { key: FOLD_KEY.into(), folded: false });
    assert_eq!(sets(&host), 2, "opening it brings the numbers up to date");
}

#[test]
fn a_hidden_window_costs_nothing_to_draw() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    plugin.on_event(&mut host, &Event::WindowShown { shown: false });
    host.calls.clear();
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snapshot(&hub, "phone") });
    assert!(host.calls.iter().all(|c| !c.starts_with("set_ui_props")));
    plugin.on_event(&mut host, &Event::WindowShown { shown: true });
    assert!(props(&host)["rows"].as_array().is_some_and(|r| !r.is_empty()), "what came meanwhile is there when it shows again");
}

#[test]
fn with_no_stream_the_card_says_how_to_get_numbers() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.streaming = false;
    plugin.on_stream_stop(&mut host);
    let p = props(&host);
    assert_eq!((p["rows"].as_array().unwrap().len(), p["empty-text"].as_str()), (0, Some("Not streaming")));
    assert_eq!(p["empty-detail"], "Start a stream to see its numbers.", "the sentence is the tooltip's");
}

#[test]
fn with_several_streams_it_follows_the_one_the_panels_show() {
    let hub = Hub::new();
    let a = hub.register("phone-a", 0);
    let b = hub.register("phone-b", 1);
    a.received(10, 0);
    b.received(25, 0);
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + SECOND, None, None);
    let (mut plugin, mut host) = set_up(snap);
    host.focused = Some("phone-b".into());
    plugin.on_focus_gained(&mut host, &telescope_core::plugin::StreamInfo { source_id: "phone-b".into(), url: String::new(), session: 1 });
    let p = props(&host);
    assert_eq!(p["source-name"], "Stream on virtual camera 1");
    assert!(row(&p, "Frames in")["value"].as_str().unwrap().starts_with("2"), "the second's 25 frames, not the first's 10");
}

#[test]
fn the_view_is_saved() {
    let mut plugin = StatsPlugin::new();
    assert_eq!(plugin.get_config(), json!({"view": "standard"}));
    plugin.set_config(&json!({"view": "nerd"})).unwrap();
    assert_eq!(plugin.view(), View::Nerd);
    assert_eq!(plugin.get_config(), json!({"view": "nerd"}));
    plugin.set_config(&json!({"view": 7})).unwrap();
    assert_eq!(plugin.view(), View::Standard);
}

#[test]
fn a_plugin_is_named_for_people() {
    assert_eq!((plugin_label("privacy_blur").as_str(), plugin_label("view").as_str(), plugin_label("").as_str()), ("Privacy blur", "View", ""));
}

fn source_with(f: impl FnOnce(&mut SourceSnapshot)) -> SourceSnapshot {
    let mut s = SourceSnapshot { target_fps: 30, seconds: 20, now: telescope_core::stats::Rates { received_fps: 30.0, ..Default::default() }, ..Default::default() };
    s.link.route = Route::Network;
    f(&mut s);
    s
}

#[test]
fn a_graph_ends_at_the_right_edge_and_keeps_a_spike_to_the_top() {
    let g = graph(&[1.0, 1.0, 30.0], 30.0);
    let points: Vec<&str> = g.split_whitespace().collect();
    assert_eq!(points.len(), 9);
    assert_eq!(points[0], "M");
    assert_eq!(points[7], "59.5");
    assert_eq!(points[8], "1.0");
    assert!(graph(&[5.0], 30.0).is_empty());
    assert!(graph(&[], 30.0).is_empty());
}

#[test]
fn a_steady_number_stays_low_under_its_floor() {
    let g = graph(&[1.0, 1.0], 30.0);
    let ys: Vec<f32> = g.split_whitespace().filter_map(|w| w.parse().ok()).skip(1).step_by(2).collect();
    assert!(ys.iter().all(|y| *y > 10.0));
}

#[test]
fn rows_carry_a_graph_once_there_is_history() {
    let hub = Hub::new();
    let snap = snapshot(&hub, "phone");
    let (_p, host) = set_up(snap);
    assert_eq!(row(&props(&host), "Frames in")["graph"], "");
    let hub = Hub::new();
    let mut snap = snapshot(&hub, "phone");
    snap.sources[0].series.received_fps = vec![30.0, 12.0, 30.0];
    let (_p, host) = set_up(snap);
    assert!(row(&props(&host), "Frames in")["graph"].as_str().unwrap().starts_with("M "));
}

#[test]
fn the_summary_gives_min_average_max_and_when() {
    let snap = Snapshot::default();
    let s = source_with(|s| s.series.received_fps = vec![30.0, 10.0, 20.0, 30.0]);
    let lines = summary(&snap, &s, None);
    let (_, line) = lines.iter().find(|(k, _)| k == "Stats frames in").unwrap();
    assert_eq!(line, "min 10.0, average 22.5, max 30.0 fps, the max 0 s ago, over 4 s");
    assert!(!lines.iter().any(|(k, _)| k == "Stats verdict"));
    assert!(!lines.iter().any(|(k, _)| k == "Stats bitrate"));
}

#[test]
fn only_the_nerd_view_adds_to_copy_diagnostics() {
    let hub = Hub::new();
    let (mut plugin, _host) = set_up(snapshot(&hub, "phone"));
    assert!(plugin.diagnostics().is_empty());
    plugin.set_config(&json!({"view": "nerd"})).unwrap();
    assert!(plugin.diagnostics().iter().any(|(k, _)| k == "Stats stream"));
}

// ── The phone's side ─────────────────────────────────────────────────────────

fn new_phone_state() -> Value {
    json!({
        "takes_stats": true, "phone_fps": 30, "camera_fps": 29.6, "battery_temp_c": 36.5,
        "wifi_band": "5", "wifi_rssi": -58, "wifi_mbps": 866, "heat_fps": 24,
        "stats": {"encode_ms": 8.4, "encode_max_ms": 15.0, "queue_ms": 3.0, "encoder_fps": 29.5, "thermal": 3, "thermal_headroom": 0.95}
    })
}

#[test]
fn a_new_phones_state_reads_into_numbers() {
    let p = PhoneReading::from_state(&new_phone_state()).unwrap();
    assert!(p.takes_stats);
    assert_eq!((p.wifi_band.as_deref(), p.wifi_rssi, p.wifi_mbps), (Some("5"), Some(-58), Some(866)));
    assert_eq!((p.encode_ms, p.queue_ms, p.thermal, p.heat_fps), (Some(8.4), Some(3.0), Some(3), Some(24)));
    assert_eq!(p.phone_delay_ms(), Some(11.4));
}

#[test]
fn an_older_phone_has_no_stats_and_the_rest_still_reads() {
    let p = PhoneReading::from_state(&json!({"camera_fps": 30.0, "battery_temp_c": 31.0, "wifi_band": "2.4", "wifi_rssi": -70, "phone_fps": 30})).unwrap();
    assert!(!p.takes_stats);
    assert_eq!((p.encode_ms, p.thermal, p.phone_delay_ms()), (None, None, None));
    assert_eq!(p.battery_temp_c, Some(31.0));
    assert!(PhoneReading::from_state(&json!({})).is_none(), "a state with nothing to show says nothing");
}

#[test]
fn nonsense_in_the_state_is_left_out() {
    let p = PhoneReading::from_state(&json!({"battery_temp_c": 30.0, "wifi_rssi": true, "wifi_mbps": -1, "stats": {"encode_ms": "fast", "thermal": 99, "queue_ms": -4.0}})).unwrap();
    assert_eq!((p.wifi_rssi, p.wifi_mbps, p.encode_ms, p.thermal, p.queue_ms), (None, None, None, None, None));
    assert!(PhoneReading::from_state(&json!({"stats": "x", "battery_temp_c": 30.0})).is_some());
}

fn phone_props(state: Value, nerd: bool) -> Value {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    if nerd {
        plugin.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "view-picked".into(), args: vec![json!(1)] });
    }
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state });
    props(&host)
}

#[test]
fn the_phone_section_shows_what_the_phone_said_instead_of_not_available() {
    let p = phone_props(new_phone_state(), false);
    assert!(!has_row(&p, "From the phone"));
    assert_eq!(row(&p, "Delay on the phone")["value"], "11 ms");
    assert_eq!(row(&p, "Delay on the phone")["note"], "from the camera 8.4 ms, waiting to go out 3.0 ms");
    assert_eq!(row(&p, "Phone's Wi-Fi")["value"], "5 GHz, -58 dBm");
    assert_eq!(row(&p, "Phone's Wi-Fi")["note"], "link speed 866 Mbps");
    assert_eq!(row(&p, "Temperature")["value"], "36.5 °C");
    let throttling = row(&p, "Throttling");
    assert_eq!((throttling["value"].as_str(), throttling["kind"].as_str()), (Some("Yes, severe"), Some("status_warn")));
    assert!(throttling["note"].as_str().unwrap().contains("24 fps"));
    assert!(!has_row(&p, "From the camera"), "Nerd has the rest");
}

#[test]
fn nerd_adds_the_encoder_and_the_camera() {
    let p = phone_props(new_phone_state(), true);
    assert_eq!(row(&p, "Camera frame rate")["note"], "asked for 30.0 fps");
    assert_eq!(row(&p, "Encoder frame rate")["value"], "29.5 fps");
    assert_eq!(row(&p, "From the camera")["value"], "8.4 ms");
    assert_eq!(row(&p, "From the camera")["note"], "slowest 15 ms");
    assert_eq!(row(&p, "Send queue")["value"], "3.0 ms");
    assert!(!has_row(&p, "Round trip, jitter and clock offset"), "what the protocol doesn't carry yet has no line");
}

#[test]
fn an_older_phone_says_it_needs_an_update_for_the_delay_and_skips_what_it_cant_give() {
    let p = phone_props(json!({"camera_fps": 30.0, "battery_temp_c": 31.0}), false);
    assert_eq!(row(&p, "Delay on the phone")["value"], "Needs a newer phone app");
    assert!(!has_row(&p, "Phone's Wi-Fi") && !has_row(&p, "Throttling"), "no Wi-Fi on USB and no thermal status before Android 10");
    assert_eq!(row(&p, "Temperature")["value"], "31.0 °C");
}

#[test]
fn a_cool_phone_is_not_throttling() {
    let p = phone_props(json!({"battery_temp_c": 30.0, "stats": {"encode_ms": 5.0, "thermal": 0}}), false);
    let throttling = row(&p, "Throttling");
    assert_eq!((throttling["value"].as_str(), throttling["kind"].as_str()), (Some("No"), Some("status_ok")));
}

#[test]
fn the_nerd_summary_carries_the_phones_numbers() {
    let hub = Hub::new();
    let snap = snapshot(&hub, "phone");
    let p = PhoneReading::from_state(&new_phone_state()).unwrap();
    let lines = summary(&snap, &snap.sources[0], Some(&p));
    let get = |k: &str| lines.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("no {k}"));
    assert_eq!(get("Stats phone delay"), "11.4 ms on the phone (from the camera 8.4, waiting to go out 3.0, slowest 15.0)");
    assert_eq!(get("Stats phone wifi"), "5 GHz, -58 dBm, 866 Mbps");
    assert_eq!(get("Stats phone thermal status"), "3 (severe), capped at 24 fps");
    assert!(get("Stats phone frame rates").contains("camera 29.6 fps, asked 30, encoder 29.5 fps"));
}

fn controls(host: &FakeHost) -> Vec<String> {
    host.calls.iter().filter(|c| c.starts_with("phone_control")).cloned().collect()
}

#[test]
fn the_phone_is_only_asked_while_the_card_is_up_with_a_stream_and_told_to_stop() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    // No stream, nothing asked
    plugin.on_event(&mut host, &Event::WindowShown { shown: true });
    assert!(controls(&host).is_empty() && host.scheduled.is_empty());
    host.streaming = true;
    plugin.on_stream_start(&mut host, &telescope_core::plugin::StreamInfo { source_id: "phone".into(), url: String::new(), session: 1 });
    assert!(host.calls.iter().any(|c| c.starts_with("request_phone_state")), "it first learns what the phone takes");
    assert!(controls(&host).is_empty(), "and sends nothing before it knows");
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: new_phone_state() });
    assert_eq!(controls(&host), [r#"phone_control Some("phone") {"action":"stats","value":1}"#]);
    // The lease is renewed on the timer
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: "phone".into() });
    assert_eq!(controls(&host).len(), 2);
    assert!(host.scheduled.iter().any(|(_, t)| t == "phone"));
    // Hiding the window lets go
    plugin.on_event(&mut host, &Event::WindowShown { shown: false });
    assert_eq!(controls(&host).last().unwrap(), r#"phone_control Some("phone") {"action":"stats","value":0}"#);
    assert!(host.scheduled.iter().all(|(_, t)| t != "phone"));
    let sent = controls(&host).len();
    plugin.on_event(&mut host, &Event::WindowShown { shown: false });
    assert_eq!(controls(&host).len(), sent, "told once");
}

#[test]
fn an_older_phone_is_never_sent_the_control() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.streaming = true;
    plugin.on_stream_start(&mut host, &telescope_core::plugin::StreamInfo { source_id: "phone".into(), url: String::new(), session: 1 });
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: json!({"camera_fps": 30.0, "battery_temp_c": 30.0}) });
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: "phone".into() });
    plugin.on_event(&mut host, &Event::WindowShown { shown: false });
    assert!(controls(&host).is_empty());
}

#[test]
fn a_stream_ending_clears_the_phones_numbers() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: new_phone_state() });
    plugin.on_stream_stop(&mut host);
    assert!(!has_row(&props(&host), "Delay on the phone"));
}

fn stream(id: &str) -> telescope_core::plugin::StreamInfo {
    telescope_core::plugin::StreamInfo { source_id: id.into(), url: String::new(), session: 1 }
}

fn streaming_with_lease() -> (StatsPlugin, FakeHost) {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.streaming = true;
    plugin.on_stream_start(&mut host, &stream("phone"));
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: new_phone_state() });
    assert_eq!(controls(&host).len(), 1);
    (plugin, host)
}

#[test]
fn folding_the_card_lets_the_phone_go_and_unfolding_asks_again() {
    let (mut plugin, mut host) = streaming_with_lease();
    plugin.on_event(&mut host, &Event::CardFolded { key: "other/Card".into(), folded: true });
    assert_eq!(controls(&host).len(), 1, "another card's fold is none of its business");
    plugin.on_event(&mut host, &Event::CardFolded { key: "stats/Stats".into(), folded: true });
    assert_eq!(controls(&host).last().unwrap(), r#"phone_control Some("phone") {"action":"stats","value":0}"#);
    assert!(host.scheduled.iter().all(|(_, t)| t != "phone"));
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: "phone".into() });
    assert_eq!(controls(&host).len(), 2, "no renewal while folded");
    plugin.on_event(&mut host, &Event::CardFolded { key: "stats/Stats".into(), folded: false });
    assert!(host.scheduled.iter().any(|(_, t)| t == "phone"));
}

#[test]
fn a_card_that_starts_folded_never_asks_the_phone() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    plugin.on_event(&mut host, &Event::CardFolded { key: "stats/Stats".into(), folded: true });
    host.streaming = true;
    plugin.on_stream_start(&mut host, &stream("phone"));
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: new_phone_state() });
    assert!(controls(&host).is_empty() && host.scheduled.is_empty());
}

#[test]
fn switching_phones_lets_the_old_one_go_and_clears_its_numbers() {
    let (mut plugin, mut host) = streaming_with_lease();
    host.live.push("phone".into());
    host.focused = Some("other".into());
    plugin.on_focus_gained(&mut host, &stream("other"));
    assert_eq!(controls(&host).last().unwrap(), r#"phone_control Some("phone") {"action":"stats","value":0}"#);
    assert!(!has_row(&props(&host), "Delay on the phone"), "the old phone's rows are gone");
    let sent = controls(&host).len();
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: json!({"camera_fps": 30.0, "battery_temp_c": 30.0}) });
    assert_eq!(controls(&host).len(), sent, "an older phone is not sent a control it doesn't take");
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: "phone".into() });
    assert_eq!(controls(&host).len(), sent);
}

#[test]
fn the_new_phone_is_the_one_asked() {
    let (mut plugin, mut host) = streaming_with_lease();
    host.focused = Some("other".into());
    plugin.on_focus_gained(&mut host, &stream("other"));
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: new_phone_state() });
    assert_eq!(controls(&host).last().unwrap(), r#"phone_control Some("other") {"action":"stats","value":1}"#);
}

#[test]
fn diagnostics_follow_the_stream_the_phone_numbers_belong_to() {
    let hub = Hub::new();
    let a = hub.register("phone-a", 0);
    let b = hub.register("phone-b", 1);
    a.received(10, 0);
    b.received(25, 0);
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + SECOND, None, None);
    let (mut plugin, mut host) = set_up(snap);
    host.focused = Some("phone-b".into());
    plugin.on_focus_gained(&mut host, &stream("phone-b"));
    plugin.set_config(&json!({"view": "nerd"})).unwrap();
    let stream_line = plugin.diagnostics().into_iter().find(|(k, _)| k == "Stats stream").unwrap().1;
    assert!(stream_line.contains("25 frames in"), "{stream_line}");
}

#[test]
fn the_phone_graph_is_one_value_for_each_second() {
    let now = 200 * SECOND;
    // A reading, then two in one second, a gap, and one: the slowest of a second wins and the quiet seconds up to now repeat the last value
    let samples = [(now - 5 * SECOND, 4.0), (now - 5 * SECOND + 300 * MS, 9.0), (now - 5 * SECOND + 600 * MS, 5.0), (now - 3 * SECOND, 6.0)];
    assert_eq!(delay_series(&samples, now), [4.0, 9.0, 6.0, 6.0, 6.0, 6.0]);
    assert!(delay_series(&[(now - 120 * SECOND, 3.0)], now).is_empty(), "older than the graph is left out");
    assert!(delay_series(&[], now).is_empty());
}

#[test]
fn active_heat_limiting_is_throttling_even_while_android_says_cool() {
    let p = phone_props(json!({"battery_temp_c": 38.0, "heat_fps": 24, "stats": {"encode_ms": 5.0, "thermal": 1, "thermal_headroom": 0.95}}), false);
    let throttling = row(&p, "Throttling");
    assert_eq!((throttling["value"].as_str(), throttling["kind"].as_str(), throttling["note"].as_str()), (Some("Yes"), Some("status_warn"), Some("capped at 24 fps")));
}

#[test]
fn the_cards_own_polls_update_the_phone_numbers() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.streaming = true;
    plugin.on_stream_start(&mut host, &telescope_core::plugin::StreamInfo { source_id: "phone".into(), url: String::new(), session: 1 });
    plugin.on_event(&mut host, &Event::PhoneStateUpdated { state: json!({"takes_stats": true, "camera_fps": 30.0, "battery_temp_c": 30.0}) });
    // What request_phone_state asked for comes back as a poll of the shown stream, and fills in the measured rows
    plugin.on_event(&mut host, &Event::PhonePolled { source_id: "phone".into(), session: 1, shown: true, state: new_phone_state() });
    let rows = phone_props(new_phone_state(), false);
    assert!(rows.to_string().contains("Delay on the phone"));
    assert_eq!(plugin.phone.as_ref().and_then(|p| p.encode_ms), Some(8.4));
    // A poll of a stream that isn't shown is somebody else's
    plugin.on_event(&mut host, &Event::PhonePolled { source_id: "other".into(), session: 1, shown: false, state: json!({"takes_stats": true, "battery_temp_c": 50.0}) });
    assert_eq!(plugin.phone.as_ref().and_then(|p| p.battery_temp_c), Some(36.5));
}

fn tapped(plugin: &mut StatsPlugin, host: &mut FakeHost, id: &str, on: bool) {
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: "pin-toggled".into(), args: vec![json!(id), json!(on)] });
}

#[test]
fn a_line_with_a_star_pins_its_number_into_the_bar() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    let p = props(&host);
    // The frame rate and the throughput are the window's own pins: the lines carry their ids and are not starred until pinned
    assert_eq!(row(&p, "Frames out")["pin"], "fps");
    assert_eq!(row(&p, "Bitrate")["pin"], "throughput");
    assert_eq!(row(&p, "Frames in")["pin"], "stats.frames_in");
    assert_eq!(row(&p, "Frames in")["pinned"], false);
    assert_eq!(row(&p, "Connection")["pin"], "stats.connection", "a text line is starred too");
    tapped(&mut plugin, &mut host, "stats.frames_in", true);
    assert!(host.calls.iter().any(|c| c == "pin_to_bar stats.frames_in true"));
    assert_eq!(row(&props(&host), "Frames in")["pinned"], true);
    tapped(&mut plugin, &mut host, "stats.frames_in", false);
    assert_eq!(row(&props(&host), "Frames in")["pinned"], false);
}

#[test]
fn only_what_is_pinned_is_offered_to_the_bar_and_with_its_own_reading() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    assert!(host.bar_offers.is_empty(), "nothing of the card's is pinned: nothing to read out");
    tapped(&mut plugin, &mut host, "stats.frames_in", true);
    tapped(&mut plugin, &mut host, "stats.memory", true);
    fn by_id(host: &FakeHost, id: &str) -> BarStat {
        host.bar_offers.iter().find(|s| s.id == id).cloned().unwrap_or_else(|| panic!("no {id} in {:?}", host.bar_offers))
    }
    assert_eq!((by_id(&host, "stats.frames_in").name.as_str(), by_id(&host, "stats.frames_in").value.as_str()), ("Frames in", "2.4 fps"));
    assert_eq!(by_id(&host, "stats.frames_in").icon, "download");
    assert_eq!(by_id(&host, "stats.memory").value, "200 MB");
    // Nerd-only numbers are offered as well, whichever view is open
    tapped(&mut plugin, &mut host, "stats.decoded", true);
    assert_eq!(by_id(&host, "stats.decoded").value, "2.3 fps");
    // The window's own readouts are the window's to read
    assert!(host.bar_offers.iter().all(|s| s.id.starts_with("stats.")));
}

#[test]
fn the_bar_stops_hearing_from_the_card_when_the_stream_ends_or_the_plugin_stops() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    tapped(&mut plugin, &mut host, "stats.frames_in", true);
    assert!(!host.bar_offers.is_empty());
    host.streaming = false;
    plugin.on_stream_stop(&mut host);
    assert!(host.bar_offers.is_empty(), "no stream: nothing to read");
    plugin.on_event(&mut host, &Event::StatsUpdated { snapshot: snapshot(&hub, "phone") });
    assert!(!host.bar_offers.is_empty());
    plugin.shutdown(&mut host);
    assert!(host.bar_offers.is_empty(), "switched off: its pins vanish quietly");
}

#[test]
fn the_camera_has_a_line_and_is_locked_while_several_streams_run() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.bar_own = vec![BarStat::new("camera", "Virtual camera", "", "Phone Camera")];
    host.bar_pinned = vec!["camera".into()];
    plugin.on_event(&mut host, &Event::BarPinsChanged { pinned: vec!["camera".into()] });
    let p = props(&host);
    assert_eq!((row(&p, "Camera")["value"].as_str(), row(&p, "Camera")["pin"].as_str(), row(&p, "Camera")["pinned"].as_bool()), (Some("Phone Camera"), Some("camera"), Some(true)));
    assert_eq!(row(&p, "Camera")["locked"], false);
    host.streaming = true;
    host.streams = Some(2);
    plugin.on_event(&mut host, &Event::BarPinsChanged { pinned: vec!["camera".into()] });
    assert_eq!(row(&props(&host), "Camera")["locked"], true);
}

#[test]
fn the_app_wide_total_is_there_only_with_several_streams_and_follows_none() {
    let hub = Hub::new();
    let a = hub.register("phone-a", 0);
    let b = hub.register("phone-b", 1);
    a.received(10, 1_000_000);
    b.received(10, 2_000_000);
    let snap = hub.snapshot_at(telescope_core::time::monotonic_ns() + 10 * SECOND, Some(SECOND), Some(200 << 20));
    let (mut plugin, mut host) = set_up(snap.clone());
    host.focused = Some("phone-a".into());
    plugin.on_focus_gained(&mut host, &stream("phone-a"));
    assert!(has_row(&props(&host), "All streams"));
    tapped(&mut plugin, &mut host, "stats.total_throughput", true);
    let total = host.bar_offers.iter().find(|s| s.id == "stats.total_throughput").unwrap().value.clone();
    // Whichever stream the panels show, the total is the same
    host.focused = Some("phone-b".into());
    plugin.on_focus_gained(&mut host, &stream("phone-b"));
    assert_eq!(host.bar_offers.iter().find(|s| s.id == "stats.total_throughput").unwrap().value, total);
    let one = hub_one();
    let (_plugin, host) = set_up(one);
    assert!(!has_row(&props(&host), "All streams"));
}

fn hub_one() -> Snapshot {
    let hub = Hub::new();
    snapshot(&hub, "phone")
}

#[test]
fn the_cards_stars_match_what_is_pinned_the_camera_included() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    host.bar_own = vec![BarStat::new("camera", "Virtual camera", "", "Phone Camera")];
    // What a fresh settings file pins
    host.bar_pinned = vec!["camera".into(), "fps".into(), "throughput".into()];
    let pinned = host.bar_pinned.clone();
    plugin.on_event(&mut host, &Event::BarPinsChanged { pinned });
    let p = props(&host);
    for label in ["Camera", "Frames out", "Bitrate"] {
        assert_eq!(row(&p, label)["pinned"], true, "{label} is starred");
    }
    for label in ["Frames in", "Repeated", "Dropped", "Memory"] {
        assert_eq!(row(&p, label)["pinned"], false, "{label} is not");
    }
    // Locked while several streams run
    host.streaming = true;
    host.streams = Some(2);
    let pinned = host.bar_pinned.clone();
    plugin.on_event(&mut host, &Event::BarPinsChanged { pinned });
    let p = props(&host);
    assert_eq!((row(&p, "Camera")["pinned"].as_bool(), row(&p, "Camera")["locked"].as_bool()), (Some(true), Some(true)));
    // Several streams bring the camera in even when it was unpinned: the host says so in bar_pins
    host.bar_pinned = vec!["camera".into()];
    let pinned = host.bar_pinned.clone();
    plugin.on_event(&mut host, &Event::BarPinsChanged { pinned });
    assert_eq!(row(&props(&host), "Camera")["pinned"], true);
}

#[test]
fn every_pinnable_stat_has_an_icon_of_its_own_or_names_itself() {
    let hub = Hub::new();
    let snap = snapshot(&hub, "phone");
    let all = rows(&snap, snap.sources.first().unwrap(), View::Nerd, &Context { streams: 2, ..Context::default() });
    // The lines with a fixed place in the card wear an icon of their own; a plugin's own icon and the text readouts (a drop cause, a stage) are the exceptions
    let mut icons: Vec<(String, &'static str)> = all.iter().filter(|r| r.pin.starts_with("stats.") && r.bar_icon.is_empty() && !pin_icon(&r.pin).is_empty()).map(|r| (r.pin.clone(), pin_icon(&r.pin))).collect();
    // The phone's lines only show once a phone has said something, so their ids are checked here
    use crate::stats_phone as ph;
    for id in [ph::PIN_DELAY, ph::PIN_WIFI, ph::PIN_TEMPERATURE, ph::PIN_THROTTLING, ph::PIN_CAMERA_FPS, ph::PIN_ENCODER_FPS, ph::PIN_ENCODE, ph::PIN_QUEUE] {
        icons.push((id.into(), pin_icon(id)));
    }
    assert!(icons.iter().all(|(_, i)| !i.is_empty()), "{icons:?}");
    // The window's own pins
    icons.push(("fps".into(), "film"));
    icons.push(("throughput".into(), "gauge"));
    let mut names: Vec<&str> = icons.iter().map(|(_, i)| *i).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), icons.len(), "two stats share an icon: {icons:?}");
    // Every line that can be read has a star, in both views, and no two share an id
    let ids: Vec<&str> = all.iter().filter(|r| !r.head && r.kind != "status_dim").map(|r| r.pin.as_str()).collect();
    assert!(ids.iter().all(|id| !id.is_empty()), "a line without a star: {:?}", all.iter().filter(|r| !r.head && r.kind != "status_dim" && r.pin.is_empty()).map(|r| &r.label).collect::<Vec<_>>());
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "two lines share an id: {ids:?}");
}

#[test]
fn the_text_stats_can_be_pinned_and_read_in_the_bar() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    tapped(&mut plugin, &mut host, "stats.connection", true);
    tapped(&mut plugin, &mut host, "stats.reading_camera", true);
    let offered = |id: &str| host.bar_offers.iter().find(|s| s.id == id).cloned().unwrap_or_else(|| panic!("{id} is offered: {:?}", host.bar_offers.iter().map(|s| &s.id).collect::<Vec<_>>()));
    let connection = offered("stats.connection");
    assert!(!connection.value.is_empty() && !connection.icon.is_empty(), "{connection:?}");
    let reading = offered("stats.reading_camera");
    assert!(!reading.value.is_empty() && !reading.icon.is_empty() && reading.icon != connection.icon, "{reading:?}");
}

#[test]
fn plugins_are_listed_by_their_display_names_with_their_own_icons() {
    let mut snap = snapshot(&Hub::new(), "phone");
    let timing = NamedTiming { name: "qr_reader".into(), timing: Default::default() };
    snap.sources[0].plugins = vec![timing.clone()];
    snap.app.hooks = vec![NamedTiming { name: "mcp".into(), ..timing }];
    let looks = vec![
        PluginLook { name: "qr_reader".into(), title: "QR reader".into(), icon: "qr".into() },
        PluginLook { name: "mcp".into(), title: "MCP".into(), icon: "plug".into() },
    ];
    let all = rows(&snap, &snap.sources[0], View::Nerd, &Context { looks, ..Context::default() });
    let step = all.iter().find(|r| r.pin == "stats.step_qr_reader").expect("the frame step line");
    assert_eq!((step.label.as_str(), step.bar_icon.as_str()), ("QR reader", "qr"));
    let hook = all.iter().find(|r| r.pin == "stats.hook_mcp").expect("the events line");
    assert_eq!(hook.label, "MCP");
    // An unknown plugin keeps its id, tidied
    assert_eq!(plugin_title("privacy_blur", &[]), "Privacy blur");
}

#[test]
fn the_apps_numbers_stay_offered_while_the_focused_stream_is_not_in_the_snapshot_yet() {
    let hub = Hub::new();
    let (mut plugin, mut host) = set_up(snapshot(&hub, "phone"));
    tapped(&mut plugin, &mut host, "stats.memory", true);
    tapped(&mut plugin, &mut host, "stats.frames_in", true);
    assert!(host.bar_offers.iter().any(|s| s.id == "stats.frames_in"));
    // A second stream took the focus before the next tick: the snapshot only knows the first
    host.focused = Some("phone-b".into());
    plugin.on_focus_gained(&mut host, &stream("phone-b"));
    let ids: Vec<&str> = host.bar_offers.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["stats.memory"], "the app's own number stays, the stream's go until it reports");
}

#[test]
fn a_pinned_plugin_timing_is_still_offered_when_it_is_not_among_the_listed_slowest() {
    let hub = Hub::new();
    let now = telescope_core::time::monotonic_ns();
    hub.snapshot_at(now, Some(0), Some(0));
    hub.register("phone", 0).set_target_fps(30);
    hub.hooks().timer("quiet").add(MS);
    for i in 0..10 {
        hub.hooks().timer(&format!("busy_{i}")).add((10 + i) * MS);
    }
    let snap = hub.snapshot_at(now + SECOND, Some(SECOND), Some(0));
    let source = &snap.sources[0];
    assert!(!rows(&snap, source, View::Nerd, &Context::default()).iter().any(|r| r.pin == "stats.hook_quiet"));
    let offered = offers(&snap, Some(source), &Context::default());
    assert!(offered.iter().any(|o| o.id == "stats.hook_quiet"));
}
