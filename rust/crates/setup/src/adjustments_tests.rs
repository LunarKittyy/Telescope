use super::*;
use telescope_core::frame::Frame;
use telescope_core::media::{Codec, DecodeOptions, Decoder, Scaler};
use telescope_core::monitor::ProcessUse;
use telescope_core::stats::{DropReason, Hub};
use telescope_core::testing::FakeHost;

const SECOND: u64 = 1_000_000_000;

fn quiet() -> Seen {
    Seen { balance: BalanceSnapshot { on_card: true, ..Default::default() }, ..Default::default() }
}

fn lighter() -> EffortSnapshot {
    EffortSnapshot { level: 2, auto_level: 2, auto: true, ..Default::default() }
}

fn kinds(seen: &Seen) -> Vec<Kind> {
    items(seen).iter().map(|i| i.kind).collect()
}

/// A stream that has run for a while, `shed` of its 30 frames a second thinned out and the steps taking `step_ms`.
fn stream(shed: u64) -> Snapshot {
    let hub = Hub::new();
    let stats = hub.register("phone", 0);
    stats.set_target_fps(30);
    let began = telescope_core::time::monotonic_ns();
    let mut last = hub.snapshot_at(began, None, None);
    for second in 1..=8 {
        stats.received(30, 250_000);
        stats.decoded(30);
        stats.dropped(DropReason::Paced, shed);
        last = hub.snapshot_at(began + second * SECOND, None, None);
    }
    last
}

struct NoScaler;

impl Scaler for NoScaler {
    fn rgb(&mut self, _frame: &Frame, _w: u32, _h: u32) -> Vec<u8> {
        Vec::new()
    }
}

struct NoMedia;

impl Media for NoMedia {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Err("no".into())
    }
    fn scaler(&self) -> Box<dyn Scaler> {
        Box::new(NoScaler)
    }
}

fn plugin() -> (AdjustmentsPlugin, FakeHost) {
    let mut host = FakeHost::new();
    host.focused = Some("phone".into());
    (AdjustmentsPlugin::new(Arc::new(NoMedia)), host)
}

#[test]
fn nothing_adjusted_is_an_empty_list() {
    assert!(items(&quiet()).is_empty());
}

#[test]
fn stepped_down_effects_are_one_item_with_two_overrides() {
    let seen = Seen { effort: lighter(), ..quiet() };
    let list = items(&seen);
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].kind, &list[0].reason), (Kind::Effects, &Reason::FramesLate));
    assert_eq!(list[0].overrides, [Override::KeepFull, Override::TurnOff]);
}

#[test]
fn power_saving_is_the_reason_not_an_item_of_its_own() {
    // Nothing stepped down: the floor alone holds the effects at level 1
    let floor_only = Seen { effort: EffortSnapshot { level: 1, floor: 1, auto: true, ..Default::default() }, ..quiet() };
    let list = items(&floor_only);
    assert_eq!((list.len(), &list[0].reason), (1, &Reason::PowerSaving));
    assert!(list[0].overrides.is_empty(), "the switch for it is on the effects' own card");
    // Stepped down further than the floor: frames running late is why
    let both = Seen { effort: EffortSnapshot { level: 2, auto_level: 2, floor: 1, auto: true, ..Default::default() }, ..quiet() };
    assert_eq!(items(&both)[0].reason, Reason::FramesLate);
    // Full quality kept: only the floor is left
    let held = Seen { effort: EffortSnapshot { level: 1, auto_level: 3, floor: 1, auto: true, held: true, ..Default::default() }, ..quiet() };
    assert_eq!(items(&held)[0].reason, Reason::PowerSaving);
}

#[test]
fn the_balancer_only_counts_when_it_moved_frames_and_nothing_pins_them() {
    let moved = Seen { balance: BalanceSnapshot { on_card: false, ..Default::default() }, busiest_gpu: Some("Game".into()), ..quiet() };
    let list = items(&moved);
    assert_eq!((list[0].kind, &list[0].reason), (Kind::Balance, &Reason::CardBusy(Some("Game".into()))));
    assert_eq!(list[0].overrides, [Override::KeepOnCard, Override::TurnOff]);
    let pinned = Seen { balance: BalanceSnapshot { on_card: false, pinned: true, ..Default::default() }, ..quiet() };
    assert!(items(&pinned).is_empty());
}

#[test]
fn dynamic_shows_a_lowered_bitrate_and_frame_rate_the_phones_heat_has_no_override() {
    let phone = |bitrate: f64, dynamic: Option<i64>, heat: Option<i64>| Phone { bitrate_bps: Some(bitrate), peak_bps: 8e6, dynamic_fps: dynamic, heat_fps: heat };
    let steady = Seen { dynamic_in_use: true, phone: phone(7.5e6, None, None), ..quiet() };
    assert!(items(&steady).is_empty(), "a little under the peak isn't lowered");
    let lowered = Seen { dynamic_in_use: true, phone: phone(3e6, Some(24), None), ..quiet() };
    assert_eq!(kinds(&lowered), [Kind::Bitrate, Kind::FrameRate]);
    assert_eq!(items(&lowered)[0].overrides, [Override::FixedBitrate, Override::TurnOff]);
    let hot = Seen { dynamic_in_use: true, phone: phone(3e6, Some(24), Some(15)), ..quiet() };
    assert_eq!(kinds(&hot), [Kind::Bitrate, Kind::Heat], "the cap the phone reports is the lower one");
    assert!(items(&hot)[1].overrides.is_empty());
    let fixed = Seen { dynamic_in_use: false, phone: phone(3e6, Some(24), None), ..quiet() };
    assert!(items(&fixed).is_empty(), "with a fixed bitrate Dynamic isn't what lowers anything");
}

#[test]
fn decoding_on_the_processor_offers_the_video_settings() {
    let seen = Seen { decoding_slow: true, ..quiet() };
    assert_eq!(items(&seen)[0].overrides, [Override::OpenVideoSettings]);
}

#[test]
fn a_trimmed_stream_size_offers_the_pick_back_or_switching_it_off() {
    let seen = Seen { trimmed: Some(((1920, 1080), (3840, 2160))), ..quiet() };
    let all = items(&seen);
    assert_eq!(kinds(&seen), [Kind::StreamSize]);
    assert_eq!(all[0].overrides, [Override::KeepPickedSize, Override::TurnOff]);
    let why = why_of(&all[0].reason);
    assert!(why.contains("1920 × 1080") && why.contains("3840 × 2160"), "{why}");
}

#[test]
fn every_item_has_a_name_a_reason_and_a_tooltip_for_each_button() {
    let seen = Seen {
        effort: lighter(),
        balance: BalanceSnapshot { on_card: false, ..Default::default() },
        dynamic_in_use: true,
        phone: Phone { bitrate_bps: Some(2e6), peak_bps: 8e6, dynamic_fps: Some(20), heat_fps: None },
        decoding_slow: true,
        trimmed: Some(((1920, 1080), (3840, 2160))),
        ..Default::default()
    };
    for item in items(&seen) {
        let row = row_json(&item);
        for key in ["name", "why", "tip"] {
            assert!(!row[key].as_str().unwrap().is_empty(), "{:?} {key}", item.kind);
        }
        for (side, at) in [("a", 0), ("b", 1)] {
            let has = item.overrides.get(at).is_some();
            for part in ["icon", "label", "tip"] {
                assert_eq!(!row[format!("{side}-{part}")].as_str().unwrap().is_empty(), has, "{:?} {side}-{part}", item.kind);
            }
        }
    }
}

#[test]
fn the_banner_waits_for_the_effects_to_be_as_light_as_they_go() {
    let shedding = stream(6);
    let source = &shedding.sources[0];
    assert!(source.computer_is_slow());
    assert!(!falling_behind(false, Some(source), &lighter()), "the effects can still step down");
    let lightest = EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() };
    assert!(falling_behind(false, Some(source), &lightest));
    let off = EffortSnapshot { auto: false, ..Default::default() };
    assert!(falling_behind(false, Some(source), &off), "stepping down is off");
    let held = EffortSnapshot { auto: true, held: true, ..Default::default() };
    assert!(falling_behind(false, Some(source), &held), "full quality is kept");
    assert!(!falling_behind(false, None, &lightest), "no stream");
}

#[test]
fn the_banner_stays_off_for_a_stream_that_keeps_up_and_clears_when_frames_do() {
    let lightest = EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() };
    let steady = stream(0);
    assert!(!falling_behind(false, Some(&steady.sources[0]), &lightest));
    assert!(!falling_behind(true, Some(&steady.sources[0]), &lightest), "it clears once frames keep up");
    let under_a_tenth = stream(2);
    assert!(!falling_behind(false, Some(&under_a_tenth.sources[0]), &lightest));
    assert!(falling_behind(true, Some(&under_a_tenth.sources[0]), &lightest), "it stays while some are still shed");
}

#[test]
fn the_banner_names_the_cause_and_what_helps() {
    let say = |cause: Option<Cause>, effect: Option<&str>| behind_issue(cause.as_ref(), effect);
    let busy = say(Some(Cause::OtherApp { name: "Renderer".into(), cpu_percent: 80.0 }), None);
    assert_eq!((busy.title.as_str(), busy.kind.as_str()), ("Telescope is falling behind", "warn"));
    assert_eq!(busy.text, "Renderer is using most of the processor. Closing it will help the most.");
    assert!(say(Some(Cause::GpuBusy { name: Some("Game".into()) }), None).text.starts_with("Game is keeping the graphics card busy."));
    assert!(say(Some(Cause::GpuBusy { name: None }), None).text.starts_with("Another program is keeping the graphics card busy."));
    assert!(say(Some(Cause::BatterySaver), None).text.contains("Turning it off"));
    assert!(say(Some(Cause::OnBattery), None).text.contains("Plugging it in"));
    assert!(say(Some(Cause::LowMemory), None).text.contains("low on memory"));
    let own = say(Some(Cause::TelescopeItself), Some("People tracking"));
    assert_eq!(own.text, "Telescope's own effects are using most of the computer. Turning off People tracking will help the most.");
    assert!(say(None, None).text.starts_with("The computer is too slow"));
}

#[test]
fn costs_name_each_running_effect_dearest_first_and_skip_the_nearly_free() {
    let mut snap = stream(0);
    let timing = |name: &str, ms: f32| NamedTiming { name: name.into(), timing: telescope_core::stats::Timing { mean_ms: ms, max_ms: ms, count: 30 } };
    snap.sources[0].plugins = vec![timing("background", 6.0), timing("filters", 0.1), timing("framing", 2.5)];
    let looks = vec![PluginLook { name: "background".into(), title: "Background".into(), icon: String::new() }];
    let list = costs(&snap.sources[0], &[timing("people", 9.0)], &looks);
    let got: Vec<(&str, f32)> = list.iter().map(|c| (c.name.as_str(), c.ms)).collect();
    assert_eq!(got, [("People tracking", 9.0), ("Background", 6.0), ("Framing", 2.5)]);
}

#[test]
fn the_status_says_whether_it_keeps_up_at_what_rate() {
    let steady = stream(0);
    assert_eq!(status_line(&steady.sources[0]), ("Smooth at 30 fps".to_string(), "ok"));
    let behind = stream(6);
    let (text, kind) = status_line(&behind.sources[0]);
    assert_eq!((text.starts_with("Behind at "), kind), (true, "warn"));
}

#[test]
fn the_chip_is_there_only_while_something_is_adjusted_and_counts_them() {
    let (mut p, _) = plugin();
    let none = p.header_props(None, &[]);
    assert_eq!((none["shown"].as_bool(), none["rows"].as_array().map(Vec::len)), (Some(false), Some(0)));
    p.rows = items(&Seen { effort: lighter(), decoding_slow: true, ..quiet() });
    let two = p.header_props(Some(("Smooth at 30 fps".into(), "ok")), &[]);
    assert_eq!((two["shown"].as_bool(), two["text"].as_str(), two["short-text"].as_str()), (Some(true), Some("Adjusting · 2"), Some("2")));
}

#[test]
fn the_overrides_reach_the_host_through_its_methods_and_an_event() {
    let (mut p, mut host) = plugin();
    p.rows = items(&Seen { effort: lighter(), balance: BalanceSnapshot { on_card: false, ..Default::default() }, dynamic_in_use: true, phone: Phone { bitrate_bps: Some(2e6), peak_bps: 8e6, ..Default::default() }, ..Default::default() });
    let click = |p: &mut AdjustmentsPlugin, host: &mut FakeHost, row: u64, which: u64| {
        p.on_ui(host, &UiEvent { ui: "header".into(), callback: "override-clicked".into(), args: vec![json!(row), json!(which)] });
    };
    click(&mut p, &mut host, 0, 0);
    assert!(host.calls.contains(&"hold_effects_full true".to_string()));
    click(&mut p, &mut host, 1, 0);
    assert!(host.calls.contains(&"keep_steps_on_card true".to_string()));
    click(&mut p, &mut host, 2, 0);
    assert_eq!(host.events, [Event::FixedBitrateRequested { on: true }]);
    // Turn off is the saved switch
    host.calls.clear();
    click(&mut p, &mut host, 0, 1);
    assert_eq!(p.get_config()["effects"], false);
    assert!(host.calls.contains(&"set_effects_step_down false".to_string()) && host.calls.contains(&"schedule_save".to_string()));
}

#[test]
fn keeping_the_picked_size_asks_stream_output_and_turn_off_is_the_saved_switch() {
    let (mut p, mut host) = plugin();
    p.on_event(&mut host, &Event::StreamSizeTrimmed { trimmed: true, width: 1920, height: 1080, picked_width: 3840, picked_height: 2160 });
    assert_eq!(p.trimmed, Some(((1920, 1080), (3840, 2160))));
    p.rows = items(&Seen { trimmed: p.trimmed, ..quiet() });
    let click = |p: &mut AdjustmentsPlugin, host: &mut FakeHost, which: u64| {
        p.on_ui(host, &UiEvent { ui: "header".into(), callback: "override-clicked".into(), args: vec![json!(0), json!(which)] });
    };
    click(&mut p, &mut host, 0);
    assert_eq!(host.events, [Event::StreamSizeHeld { on: true }]);
    click(&mut p, &mut host, 1);
    assert_eq!(p.get_config()["stream_size"], false);
    p.on_event(&mut host, &Event::StreamStopped);
    assert_eq!(p.trimmed, None, "a new stream starts untrimmed");
}

#[test]
fn the_switches_are_applied_at_the_start_and_reset_puts_them_back_on() {
    let (mut p, mut host) = plugin();
    p.set_config(&json!({"effects": false, "balance": false, "dynamic": false, "stream_size": false})).unwrap();
    p.on_plugins_ready(&mut host);
    for call in ["set_effects_step_down false", "hold_effects_full false", "keep_steps_on_card true"] {
        assert!(host.calls.contains(&call.to_string()), "{call}: {:?}", host.calls);
    }
    assert_eq!(host.events, [Event::FixedBitrateRequested { on: true }, Event::StreamSizeHeld { on: true }]);
    p.on_ui(&mut host, &UiEvent { ui: SECTION.into(), callback: "reset-clicked".into(), args: vec![] });
    assert_eq!(p.get_config(), json!({"effects": true, "balance": true, "dynamic": true, "stream_size": true}));
    assert_eq!(host.events[2..], [Event::FixedBitrateRequested { on: false }, Event::StreamSizeHeld { on: false }]);
    assert!(host.calls.contains(&"keep_steps_on_card false".to_string()));
}

#[test]
fn the_computer_is_only_read_while_the_banner_shows_or_the_list_is_open() {
    let (mut p, mut host) = plugin();
    host.streaming = true;
    let wants = |host: &FakeHost, on: bool| host.calls.iter().filter(|c| **c == format!("want_system adjustments {on}")).count();
    let mut snap = stream(6);
    snap.app.effort = EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() };
    p.on_event(&mut host, &Event::StatsUpdated { snapshot: snap.clone() });
    assert_eq!(wants(&host, true), 1, "asked while the banner shows");
    assert_eq!(host.issues.len(), 1);
    // The reading arrives and the banner says who
    snap.app.system = Some(SystemReading { cpu_percent: Some(95.0), cores: 4, own_cpu_percent: Some(20.0), busiest: vec![ProcessUse { name: "Renderer".into(), percent: 70.0 }], ..Default::default() });
    p.on_event(&mut host, &Event::StatsUpdated { snapshot: snap });
    assert!(host.issues[0].1.text.starts_with("Renderer is using most of the processor."));
    assert_eq!(wants(&host, true), 1, "asked once");
    // Frames keep up again
    let mut calm = stream(0);
    calm.app.effort = EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() };
    p.on_event(&mut host, &Event::StatsUpdated { snapshot: calm });
    assert!(host.issues.is_empty());
    assert_eq!(wants(&host, false), 1, "let go once it clears");
    // The list asks too
    p.on_ui(&mut host, &UiEvent { ui: "header".into(), callback: "opened".into(), args: vec![] });
    assert_eq!(wants(&host, true), 2);
    p.on_ui(&mut host, &UiEvent { ui: "header".into(), callback: "closed".into(), args: vec![] });
    assert_eq!(wants(&host, false), 2);
}

#[test]
fn the_computer_is_never_named_in_what_is_saved_or_copied() {
    let (mut p, mut host) = plugin();
    let mut snap = stream(6);
    snap.app.effort = EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() };
    snap.app.system = Some(SystemReading { busiest: vec![ProcessUse { name: "Secret program".into(), percent: 70.0 }], cpu_percent: Some(95.0), own_cpu_percent: Some(5.0), ..Default::default() });
    p.on_event(&mut host, &Event::StatsUpdated { snapshot: snap });
    assert!(!p.get_config().to_string().contains("Secret"));
    assert!(p.diagnostics().is_empty(), "nothing of the reading goes into Copy diagnostics");
}

#[test]
fn a_stopped_stream_empties_the_list_and_takes_the_banner_down() {
    let (mut p, mut host) = plugin();
    let mut snap = stream(6);
    snap.app.effort = EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() };
    p.on_event(&mut host, &Event::StatsUpdated { snapshot: snap });
    assert!(!host.issues.is_empty() && !p.rows.is_empty());
    p.on_event(&mut host, &Event::StreamStopped);
    assert!(host.issues.is_empty() && p.rows.is_empty());
    assert!(host.calls.contains(&"want_system adjustments false".to_string()));
}

fn lightest() -> EffortSnapshot {
    EffortSnapshot { level: MOST, auto_level: MOST, auto: true, ..Default::default() }
}

fn behind_with(p: &mut AdjustmentsPlugin, host: &mut FakeHost, shed: u64, app: Option<&str>, now: u64) {
    let mut snap = stream(shed);
    snap.app.effort = lightest();
    if let Some(name) = app {
        snap.app.system = Some(SystemReading { cpu_percent: Some(95.0), cores: 4, own_cpu_percent: Some(5.0), busiest: vec![ProcessUse { name: name.into(), percent: 70.0 }], ..Default::default() });
    }
    p.on_stats_at(host, &snap, now);
}

fn dismiss(p: &mut AdjustmentsPlugin, host: &mut FakeHost) {
    telescope_core::host::HostShell::clear_issue(host, Some(BANNER), None);
    p.on_issue_action(host, BANNER, "dismiss");
}

#[test]
fn a_closed_banner_stays_closed_while_the_cause_stays_the_same() {
    let (mut p, mut host) = plugin();
    behind_with(&mut p, &mut host, 6, Some("Renderer"), 0);
    assert_eq!(host.issues.len(), 1);
    dismiss(&mut p, &mut host);
    for second in 1..30 {
        behind_with(&mut p, &mut host, 6, Some("Renderer"), second * SECOND);
        assert!(host.issues.is_empty(), "still closed at {second} s");
    }
    assert!(p.asking, "the computer is still read to tell whether it gets worse");
}

#[test]
fn a_closed_banner_comes_back_for_a_different_cause_or_clearly_worse_frames() {
    let (mut p, mut host) = plugin();
    behind_with(&mut p, &mut host, 6, Some("Renderer"), 0);
    dismiss(&mut p, &mut host);
    behind_with(&mut p, &mut host, 6, Some("Other thing"), SECOND);
    assert_eq!(host.issues.len(), 1, "another program is a new cause");
    dismiss(&mut p, &mut host);
    behind_with(&mut p, &mut host, 6, Some("Other thing"), 2 * SECOND);
    assert!(host.issues.is_empty());
    behind_with(&mut p, &mut host, 20, Some("Other thing"), 3 * SECOND);
    assert_eq!(host.issues.len(), 1, "far more frames shed is clearly worse");
}

#[test]
fn a_closed_banner_is_forgotten_after_a_long_quiet_spell() {
    let (mut p, mut host) = plugin();
    behind_with(&mut p, &mut host, 6, Some("Renderer"), 0);
    dismiss(&mut p, &mut host);
    let mut calm = stream(0);
    calm.app.effort = lightest();
    p.on_stats_at(&mut host, &calm, SECOND);
    behind_with(&mut p, &mut host, 6, Some("Renderer"), 10 * SECOND);
    assert!(host.issues.is_empty(), "a short break is the same episode");
    p.on_stats_at(&mut host, &calm, 11 * SECOND);
    p.on_stats_at(&mut host, &calm, 11 * SECOND + QUIET_NS);
    behind_with(&mut p, &mut host, 6, Some("Renderer"), 12 * SECOND + QUIET_NS);
    assert_eq!(host.issues.len(), 1, "a new episode shows it again");
}

#[test]
fn the_banner_asks_to_hear_about_its_dismissal() {
    assert!(behind_issue(None, None).tell_dismiss);
}

#[test]
fn every_switch_in_settings_is_one_the_plugin_has() {
    let ui = include_str!("../ui/adjustments.slint");
    assert_eq!(ui.matches("SwitchRow {").count(), 3);
    assert!(!ui.contains("enabled: false"));
}
