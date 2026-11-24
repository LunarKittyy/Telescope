use super::*;
use crate::balance::Balance;
use crate::effort::Effort;

const MS: u64 = 1_000_000;

/// A stream registered with a hub of its own, and the time its counting began at.
fn running() -> (Hub, Arc<SourceStats>, u64) {
    let hub = Hub::new();
    let stats = hub.register("phone", 2);
    let began = stats.started_ns;
    (hub, stats, began)
}

/// What a second of a stream looks like: the numbers a worker would count.
fn a_second(stats: &SourceStats) {
    stats.received(30, 250_000);
    stats.decoded(29);
    for _ in 0..27 {
        stats.delivered();
    }
    for _ in 0..3 {
        stats.repeated();
    }
}

#[test]
fn every_kind_of_frame_is_its_own_count() {
    let (hub, stats, began) = running();
    a_second(&stats);
    let snap = hub.snapshot_at(began + SECOND, None, None);
    let s = &snap.sources[0];
    assert_eq!((s.source_id.as_str(), s.slot), ("phone", 2));
    assert_eq!(s.totals, Counts { received: 30, bytes: 250_000, decoded: 29, delivered: 27, repeated: 3 });
    assert_eq!((s.now.received_fps, s.now.decoded_fps, s.now.delivered_fps, s.now.repeated_fps), (30.0, 29.0, 27.0, 3.0));
    assert_eq!(s.now.mbps, 2.0);
    // The next second is measured on its own, and the totals go on
    stats.received(10, 125_000);
    let snap = hub.snapshot_at(began + 3 * SECOND, None, None);
    let s = &snap.sources[0];
    assert_eq!((s.totals.received, s.now.received_fps, s.now.delivered_fps, s.now.mbps), (40, 5.0, 0.0, 0.5));
    assert_eq!(s.series.received_fps, [30.0, 5.0]);
    assert_eq!(s.seconds, 3);
}

#[test]
fn each_drop_is_counted_under_its_reason() {
    let (hub, stats, began) = running();
    for (reason, frames) in [
        (DropReason::LostOnLink, 4), (DropReason::Late, 1), (DropReason::DecoderFailed, 2), (DropReason::WaitingForKeyframe, 6),
        (DropReason::Paced, 9), (DropReason::StepFailed, 3), (DropReason::Unknown, 5),
    ] {
        stats.dropped(reason, frames);
    }
    let snap = hub.snapshot_at(began + 2 * SECOND, None, None);
    let s = &snap.sources[0];
    assert_eq!(s.drops, Drops { lost_on_link: 4, late: 1, decoder_failed: 2, waiting_for_keyframe: 6, paced: 9, step_failed: 3, unknown: 5 });
    assert_eq!(s.drops.total(), 30);
    for reason in DropReason::ALL {
        assert!(s.drops.of(reason) > 0, "{reason:?}");
    }
    assert_eq!(s.now.dropped_fps, 15.0);
}

#[test]
fn drops_are_recorded_on_the_thread_that_has_the_stream() {
    let (hub, stats, began) = running();
    let elsewhere = std::thread::spawn(|| {
        // Nothing is bound here: a drop with nobody to count it is nothing
        note_drop(DropReason::LostOnLink, 100);
        note_decoded(100);
        assert!(current().is_none());
    });
    elsewhere.join().unwrap();
    {
        let _counting = bind(&stats);
        note_drop(DropReason::LostOnLink, 2);
        note_decoded(5);
        let nested = Arc::new(SourceStats::detached());
        {
            let _inner = bind(&nested);
            note_drop(DropReason::Late, 7);
        }
        // Back to the first one once the inner one lets go
        note_drop(DropReason::Late, 1);
        assert_eq!(nested.drop_counts()[DropReason::Late.index()], 7);
    }
    note_drop(DropReason::Late, 50);
    let snap = hub.snapshot_at(began + SECOND, None, None);
    assert_eq!((snap.sources[0].drops.lost_on_link, snap.sources[0].drops.late, snap.sources[0].totals.decoded), (2, 1, 5));
}

#[test]
fn a_stage_that_runs_slowly_shows_its_mean_and_its_spike() {
    let (hub, stats, began) = running();
    let (decode, pipeline) = (STAGES.iter().position(|s| *s == "decode").unwrap(), STAGES.iter().position(|s| *s == "pipeline").unwrap());
    {
        let _counting = bind(&stats);
        for ms in [4, 4, 4, 28] {
            note_stage(decode, ms * MS);
        }
        note_stage(pipeline, 12 * MS);
    }
    let snap = hub.snapshot_at(began + SECOND, None, None);
    let s = &snap.sources[0];
    let decode = s.stages.iter().find(|t| t.name == "decode").unwrap().timing;
    assert_eq!((decode.count, decode.mean_ms, decode.max_ms), (4, 10.0, 28.0));
    assert_eq!(s.stages.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["decode", "pipeline"], "only what ran, in a frame's order");
    assert_eq!((s.series.decode_ms.clone(), s.series.step_ms.clone(), s.series.camera_ms.clone()), (vec![28.0], vec![12.0], vec![0.0]));
    // A quiet second after it: no times, but the graph still has the spike
    let snap = hub.snapshot_at(began + 2 * SECOND, None, None);
    assert!(snap.sources[0].stages.is_empty());
    assert_eq!(snap.sources[0].series.decode_ms, [28.0, 0.0]);
}

#[test]
fn a_perf_stage_reaches_the_stream_the_thread_counts_for() {
    let (hub, stats, began) = running();
    {
        let _counting = bind(&stats);
        crate::perf::record("upload", MS);
    }
    crate::perf::record("upload", 9 * MS);
    let snap = hub.snapshot_at(began + SECOND, None, None);
    let upload = snap.sources[0].stages.iter().find(|t| t.name == "upload").unwrap().timing;
    assert_eq!((upload.count, upload.max_ms), (1, 1.0));
}

#[test]
fn the_expensive_numbers_are_only_measured_while_something_wants_them() {
    let (hub, stats, began) = running();
    assert!(!hub.detailed() && !stats.steps().on() && !hub.hooks().on());
    // A pipeline looks at on() before it times a step, so with nobody asking nothing is added
    let blur = stats.steps().timer("privacy_blur");
    if stats.steps().on() {
        blur.add(5 * MS);
    }
    assert!(hub.snapshot_at(began + SECOND, None, None).sources[0].plugins.is_empty());
    hub.want("stats", true);
    hub.want("other", true);
    assert!(hub.detailed() && stats.steps().on() && hub.hooks().on());
    for ms in [2, 6] {
        if stats.steps().on() {
            blur.add(ms * MS);
        }
    }
    if hub.hooks().on() {
        hub.hooks().timer("lua_thing").add(3 * MS);
    }
    let snap = hub.snapshot_at(began + 2 * SECOND, None, None);
    assert!(snap.detailed);
    let plugin = &snap.sources[0].plugins[0];
    assert_eq!((plugin.name.as_str(), plugin.timing), ("privacy_blur", Timing { mean_ms: 4.0, max_ms: 6.0, count: 2 }));
    assert_eq!(snap.app.hooks, [NamedTiming { name: "lua_thing".into(), timing: Timing { mean_ms: 3.0, max_ms: 3.0, count: 1 } }]);
    // Each is taken once, and one owner letting go isn't enough
    hub.want("stats", false);
    assert!(hub.detailed());
    hub.want("other", false);
    assert!(!hub.detailed() && !stats.steps().on());
    assert!(hub.snapshot_at(began + 3 * SECOND, None, None).sources[0].plugins.is_empty());
}

#[test]
fn a_minute_of_every_series_is_kept_and_no_more() {
    let (hub, stats, began) = running();
    let mut snap = Snapshot::default();
    for second in 1..=75u64 {
        stats.received(second, 0);
        snap = hub.snapshot_at(began + second * SECOND, Some(second * SECOND / 2), Some(100 << 20));
    }
    let s = &snap.sources[0];
    assert_eq!(s.series.received_fps.len(), KEPT);
    // The oldest second left is the 16th of 75: 60 seconds kept, each its own rate
    assert_eq!(s.series.received_fps.first(), Some(&16.0));
    assert_eq!(s.series.received_fps.last(), Some(&75.0));
    for series in [&s.series.decoded_fps, &s.series.delivered_fps, &s.series.repeated_fps, &s.series.dropped_fps, &s.series.mbps, &s.series.decode_ms, &s.series.step_ms, &s.series.camera_ms] {
        assert_eq!(series.len(), KEPT);
    }
    assert_eq!(snap.app.series.memory_mb.len(), KEPT);
    assert_eq!(snap.app.series.cpu_percent.len(), KEPT.min(74));
}

#[test]
fn the_apps_cpu_is_the_share_of_one_core_it_used_since_the_last_look() {
    let hub = Hub::new();
    let first = hub.snapshot_at(10 * SECOND, Some(4 * SECOND), Some(300 << 20));
    assert_eq!((first.app.cpu_percent, first.app.memory_mb), (None, Some(300.0)), "nothing to compare with yet");
    let second = hub.snapshot_at(11 * SECOND, Some(4 * SECOND + 750 * MS), Some(310 << 20));
    assert_eq!((second.app.cpu_percent, second.app.memory_mb), (Some(75.0), Some(310.0)));
    let busy = hub.snapshot_at(13 * SECOND, Some(8 * SECOND + 750 * MS), None);
    assert_eq!((busy.app.cpu_percent, busy.app.memory_mb), (Some(200.0), None), "two cores' worth");
    assert_eq!(busy.app.series.cpu_percent, [75.0, 200.0]);
}

#[cfg(target_os = "linux")]
#[test]
fn the_system_tells_what_this_process_costs() {
    let (cpu, memory) = process_reading();
    // The clock ticks every 10 ms, so a test this short may have used none of it
    assert!(cpu.is_some(), "{cpu:?}");
    assert!(memory.is_some_and(|m| m > (1 << 20)), "{memory:?}");
}

#[test]
fn the_link_is_read_from_the_video_url() {
    let of = |url: &str| LinkInfo::of_url(url, false);
    assert_eq!(of("https://127.0.0.1:40001/v1/video.h264"), LinkInfo { route: Route::Usb, transport: Transport::Tcp });
    assert_eq!(of("https://192.168.1.20:8443/v1/video.udp"), LinkInfo { route: Route::Network, transport: Transport::Udp });
    assert_eq!(of("https://100.101.102.103:8443/v1/video"), LinkInfo { route: Route::Network, transport: Transport::Tcp });
    assert_eq!(of("https://[::1]:9/v1/video.udp").route, Route::Usb);
    assert_eq!(of("https://phone.local/v1/video.h264?x=1").route, Route::Network);
    assert_eq!(of(""), LinkInfo::default());
    assert_eq!(LinkInfo::of_url("webcam:/dev/video0", true), LinkInfo { route: Route::Local, transport: Transport::Local });
}

#[test]
fn the_balancers_moves_say_when_and_why() {
    let balance = Arc::new(Balance::free());
    let hub = Hub::with_balance(balance.clone());
    let quiet = hub.snapshot_at(monotonic_ns(), None, None).app.balance;
    assert_eq!((quiet.on_card, quiet.pinned, quiet.moves, quiet.recent.len()), (true, false, 0, 0));
    // A busy card: late frames until the frame steps leave it
    for _ in 0..6 {
        balance.note_wait(25 * MS);
    }
    let snap = hub.snapshot_at(monotonic_ns() + 2 * SECOND, None, None).app.balance;
    assert!(!snap.on_card && snap.moves == 1);
    let moved = snap.recent[0];
    assert_eq!((moved.to_card, moved.why, moved.wait_ms), (false, Why::CardBusy, 25.0));
    assert!((1.9..2.1).contains(&moved.seconds_ago), "{}", moved.seconds_ago);
    assert_eq!(snap.last_wait_ms, 25.0);
    let pinned = Hub::with_balance(Arc::new(Balance::pinned_to(Place::Cpu))).snapshot_at(1, None, None).app.balance;
    assert!(pinned.pinned && !pinned.on_card);
}

#[test]
fn a_snapshot_is_numbers_that_cross_the_bus_and_come_back() {
    let (hub, stats, began) = running();
    a_second(&stats);
    stats.dropped(DropReason::Late, 2);
    let snap = hub.snapshot_at(began + SECOND, Some(SECOND), Some(200 << 20));
    let event = Event::StatsUpdated { snapshot: snap.clone() };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["event"], "stats_updated");
    assert_eq!(json["snapshot"]["sources"][0]["drops"]["late"], 2);
    assert_eq!(json["snapshot"]["sources"][0]["link"]["route"], "unknown");
    assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
}

#[test]
fn a_stream_that_ends_leaves_the_snapshot_and_the_panel_hears_it_once() {
    let (hub, stats, began) = running();
    assert!(hub.worth_sending());
    assert_eq!(hub.snapshot_at(began + SECOND, None, None).sources.len(), 1);
    hub.unregister(&stats);
    assert!(hub.worth_sending(), "one more, empty, so the panel clears");
    assert!(hub.snapshot_at(began + 2 * SECOND, None, None).sources.is_empty());
    assert!(!hub.worth_sending());
}

#[test]
fn the_hub_sends_snapshots_to_the_bus_while_a_stream_runs() {
    let bus = crate::EventBus::new();
    let hub = Arc::new(Hub::new());
    hub.start(bus.sender());
    assert!(bus.is_empty(), "nothing to say before a stream");
    let stats = hub.register("phone", 0);
    stats.received(1, 1000);
    std::thread::sleep(std::time::Duration::from_millis(EVERY_MS + 600));
    let Some(Event::StatsUpdated { snapshot }) = bus.next() else { panic!("no snapshot after a second") };
    assert_eq!(snapshot.sources[0].totals.received, 1);
}

#[test]
fn a_feed_that_fell_back_says_which_transport_it_really_uses() {
    let (hub, stats, began) = running();
    stats.set_link("https://192.168.1.20:8443/v1/video.udp", false);
    {
        let _counting = bind(&stats);
        note_transport(Transport::Tcp);
    }
    assert_eq!(hub.snapshot_at(began + SECOND, None, None).sources[0].link, LinkInfo { route: Route::Network, transport: Transport::Tcp });
    // A new route starts from what its URL says
    stats.set_link("https://192.168.1.20:8443/v1/video.udp", false);
    assert_eq!(hub.snapshot_at(began + 2 * SECOND, None, None).sources[0].link.transport, Transport::Udp);
}

#[test]
fn a_mean_never_passes_its_slowest() {
    let timer = Timer::default();
    timer.add(3 * MS);
    timer.add(5 * MS);
    let t = timer.take();
    assert_eq!((t.count, t.mean_ms, t.max_ms), (2, 4.0, 5.0));
    // Nothing is left over to count twice
    assert_eq!(timer.take(), Timing::default());
}

#[test]
fn model_runs_are_timed_always_and_the_computer_only_while_asked() {
    let (hub, _stats, began) = running();
    crate::stats::note_model("mask", 4 * MS);
    crate::stats::note_model("mask", 2 * MS);
    crate::stats::note_model("people", 8 * MS);
    crate::stats::note_model("something else", 8 * MS);
    let snap = hub.snapshot_at(began + SECOND, None, None);
    let names: Vec<&str> = snap.app.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["mask", "people"]);
    assert_eq!((snap.app.models[0].timing.count, snap.app.models[0].timing.mean_ms), (2, 3.0));
    assert!(snap.app.system.is_none(), "nobody asked");
    assert!(hub.snapshot_at(began + 2 * SECOND, None, None).app.models.is_empty(), "a second with no runs has none");

    hub.want_system("test", true);
    assert!(hub.snapshot_at(began + 3 * SECOND, None, None).app.system.is_some());
    hub.want_system("other", true);
    hub.want_system("test", false);
    assert!(hub.snapshot_at(began + 4 * SECOND, None, None).app.system.is_some(), "one still wants it");
    hub.want_system("other", false);
    assert!(hub.snapshot_at(began + 5 * SECOND, None, None).app.system.is_none());
    // Asked again after a pause, the first reading starts over, so its rates don't average the whole pause
    hub.want_system("test", true);
    assert!(!hub.system.lock().unwrap().1.has_history());
    hub.snapshot_at(began + 6 * SECOND, None, None);
    assert!(hub.system.lock().unwrap().1.has_history());
}

#[test]
fn what_the_computer_is_doing_never_reaches_a_plugin_as_json() {
    let mut snap = Snapshot::default();
    snap.app.system = Some(crate::monitor::SystemReading { busiest: vec![crate::monitor::ProcessUse { name: "secret program".into(), percent: 50.0 }], ..Default::default() });
    let text = serde_json::to_string(&snap).unwrap();
    assert!(!text.contains("secret program") && !text.contains("system"), "{text}");
    let back: Snapshot = serde_json::from_str(&text).unwrap();
    assert!(back.app.system.is_none());
}

#[test]
fn the_snapshot_carries_the_effects_level_and_an_old_one_without_it_still_loads() {
    let effort = Arc::new(Effort::free());
    let hub = Hub::with_parts(Arc::new(Balance::free()), effort.clone());
    assert_eq!(hub.snapshot_at(1, None, None).app.effort.level, 0);
    effort.set_floor(1);
    let snap = hub.snapshot_at(2, None, None);
    assert_eq!((snap.app.effort.level, snap.app.effort.floor, snap.app.effort.moves.len()), (1, 1, 1));
    let mut json = serde_json::to_value(&snap).unwrap();
    json["app"].as_object_mut().unwrap().remove("effort");
    assert_eq!(serde_json::from_value::<Snapshot>(json).unwrap().app.effort, crate::effort::EffortSnapshot::default());
}

#[test]
fn the_hub_tells_the_bus_when_the_effects_level_changed_even_with_no_stream() {
    let bus = crate::EventBus::new();
    let effort = Arc::new(Effort::free());
    let hub = Arc::new(Hub::with_parts(Arc::new(Balance::free()), effort.clone()));
    hub.start(bus.sender());
    effort.set_floor(2);
    std::thread::sleep(std::time::Duration::from_millis(EVERY_MS + 600));
    assert_eq!(bus.next(), Some(Event::EffortChanged { level: 2 }));
    assert!(bus.is_empty(), "once per change");
}

#[test]
fn the_computer_is_slow_when_a_tenth_of_the_frames_are_shed_for_a_few_seconds() {
    use crate::stats::DropReason;
    let (hub, stats, began) = running();
    let shed_per_second = |hub: &Hub, from: u64, seconds: u64, shed: u64| {
        let mut last = None;
        for i in 0..seconds {
            stats.received(30, 250_000);
            stats.dropped(DropReason::Paced, shed);
            last = Some(hub.snapshot_at(began + (from + i) * SECOND, None, None));
        }
        last.unwrap().sources.remove(0)
    };
    let steady = shed_per_second(&hub, 1, 8, 0);
    assert_eq!((steady.shed_share(4), steady.computer_is_slow()), (Some(0.0), false));
    let two_a_second = shed_per_second(&hub, 9, 8, 2);
    assert!(!two_a_second.computer_is_slow(), "2 of 30 is under a tenth: {:?}", two_a_second.shed_share(4));
    let four_a_second = shed_per_second(&hub, 17, 8, 4);
    assert!(four_a_second.computer_is_slow(), "{:?}", four_a_second.shed_share(4));
    assert_eq!(four_a_second.shed_share(100), None, "not that many seconds counted yet");
}

#[test]
fn the_delivered_total_is_the_bound_streams_own() {
    let (_hub, stats, _) = running();
    assert_eq!(delivered_total(), None);
    let _counting = bind(&stats);
    assert_eq!(delivered_total(), Some(0));
    stats.delivered();
    stats.delivered();
    assert_eq!(delivered_total(), Some(2));
}

#[test]
fn the_steps_epoch_counts_the_bound_streams_step_changes() {
    let (_hub, stats, _) = running();
    assert_eq!(steps_epoch(), None);
    let _counting = bind(&stats);
    assert_eq!(steps_epoch(), Some(0));
    stats.steps_changed();
    assert_eq!(steps_epoch(), Some(1));
}

#[test]
fn the_rate_is_there_without_the_lock() {
    let (_hub, stats, _) = running();
    assert_eq!(stats.target_fps(), 0);
    stats.set_target_fps(60);
    assert_eq!(stats.target_fps(), 60);
}

#[test]
fn each_stream_says_where_its_own_steps_run() {
    let hub = Hub::new();
    let (a, b) = (hub.register("a", 0), hub.register("b", 1));
    a.set_steps_on_card(false);
    b.set_steps_on_card(true);
    let snap = hub.snapshot_at(crate::time::monotonic_ns() + SECOND, None, None);
    let on = |id: &str| snap.sources.iter().find(|s| s.source_id == id).unwrap().steps_on_card;
    assert_eq!((on("a"), on("b")), (Some(false), Some(true)));
    assert_eq!(hub.register("c", 2).steps_on.load(Ordering::Relaxed), STEPS_UNKNOWN, "nothing said yet");
}

fn power_hub() -> (Hub, Arc<Effort>) {
    let effort = Arc::new(Effort::free());
    (Hub::with_parts(Arc::new(Balance::free()), effort.clone()), effort)
}

#[test]
fn the_effects_follow_the_computers_power_saving_only_when_asked_to_and_only_on_a_clear_yes() {
    let (hub, effort) = power_hub();
    hub.note_power(Some(true));
    assert_eq!(effort.level(), 0, "nothing follows it until a plugin switches that on");
    hub.set_power_aware(true);
    hub.note_power(Some(true));
    assert_eq!((effort.level(), effort.snapshot().machine_floor), (1, 1));
    hub.note_power(None);
    assert_eq!(effort.level(), 0, "can't tell is no saving");
    hub.note_power(Some(true));
    hub.note_power(Some(false));
    assert_eq!(effort.level(), 0);
    // Switching it off lets go at once, whatever the computer says
    hub.note_power(Some(true));
    hub.set_power_aware(false);
    assert_eq!(effort.level(), 0);
    hub.note_power(Some(true));
    assert_eq!(effort.level(), 0);
}

#[test]
fn the_computer_is_asked_at_once_when_a_stream_starts_and_then_every_few_ticks_and_never_without_one() {
    let (hub, effort) = power_hub();
    hub.set_power_aware(true);
    let asked = std::cell::Cell::new(0);
    let tick = |streaming: bool, saving: bool| hub.follow_power(streaming, || { asked.set(asked.get() + 1); Some(saving) });
    tick(false, true);
    assert_eq!((asked.get(), effort.level()), (0, 0), "no stream, no asking");
    tick(true, true);
    assert_eq!((asked.get(), effort.level()), (1, 1), "the first tick of a stream asks");
    for _ in 0..4 {
        tick(true, false);
    }
    assert_eq!((asked.get(), effort.level()), (1, 1), "the next few ticks don't");
    tick(true, false);
    assert_eq!((asked.get(), effort.level()), (2, 0), "and then it does, and the change shows");
    // A stream that stops and starts again is asked straight away
    tick(false, false);
    tick(true, true);
    assert_eq!((asked.get(), effort.level()), (3, 1));
}
