use super::*;
use telescope_core::balance::{Balance, Place};
use std::sync::Arc;
use telescope_core::stats::{DropReason, Hub};
use telescope_core::testing::FakeHost;

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
    assert!(!has_row(&p, "Decoded") && !has_row(&p, "Memory") && !has_row(&p, "Frame steps run on"));
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
fn what_needs_the_phone_says_it_is_not_available_yet() {
    let hub = Hub::new();
    let (_plugin, host) = set_up(snapshot(&hub, "phone"));
    let p = props(&host);
    for label in ["Delay from camera to virtual camera", "Wi-Fi band and signal", "Temperature and throttling"] {
        assert_eq!((row(&p, label)["value"].as_str(), row(&p, label)["kind"].as_str()), (Some("Not available yet"), Some("status_dim")), "{label}");
    }
    assert!(!has_row(&p, "Encoding time and queue"), "that's for Nerd");
}

#[test]
fn the_bitrate_is_shown_against_what_was_asked_for() {
    let hub = Hub::new();
    let snap = snapshot(&hub, "phone");
    let source = &snap.sources[0];
    let rows_with = |asked| rows(&snap, source, View::Standard, &Context { asked_mbps: asked, watched: None });
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
    // The slowest plugin comes first
    let labels: Vec<&str> = p["rows"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap()).collect();
    let blur = labels.iter().position(|l| *l == "Privacy blur").unwrap();
    assert!(blur < labels.iter().position(|l| *l == "Colour grading").unwrap());
    assert_eq!((row(&p, "Privacy blur")["value"].as_str(), row(&p, "Privacy blur")["note"].as_str()), (Some("9.0 ms"), Some("slowest 9.0 ms")));
    assert_eq!(row(&p, "Lua thing")["value"], "3.0 ms");
    assert_eq!(row(&p, "Processor")["value"], "100% of one core");
    assert_eq!(row(&p, "Memory")["value"], "512 MB");
    assert!(has_row(&p, "Encoding time and queue"));
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
    assert_eq!((p["rows"].as_array().unwrap().len(), p["empty-text"].as_str()), (0, Some("Start a stream to see its numbers.")));
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
