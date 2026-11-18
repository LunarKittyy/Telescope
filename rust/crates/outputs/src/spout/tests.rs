use super::*;
use std::sync::atomic::AtomicU32;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

fn cam() -> Sender {
    Sender { key: "Cam".into(), name: "Cam".into(), width: 4, height: 2 }
}

fn picture(width: u32, height: u32, fourcc: &'static str, pixel: [u8; 4]) -> Picture {
    Picture { width, height, fourcc, data: pixel.iter().copied().cycle().take(width as usize * height as usize * 4).collect() }
}

/// What a reader hands out next: a picture, nothing yet, or the end.
type Script = Arc<Mutex<Vec<Result<Option<Picture>, String>>>>;

/// The stop flag the feed gave its reader.
type StopSlot = Arc<Mutex<Option<Arc<AtomicBool>>>>;

struct FakeReader(Script, StopSlot, Arc<AtomicBool>);

impl Reader for FakeReader {
    fn set_stop(&mut self, stop: Arc<AtomicBool>) {
        *self.1.lock().unwrap() = Some(stop);
    }

    fn stalled(&self) -> bool {
        self.2.load(Ordering::SeqCst)
    }

    fn next(&mut self, timeout: Duration) -> Result<Option<Picture>, String> {
        let mut script = self.0.lock().unwrap();
        if script.is_empty() {
            drop(script);
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            return Ok(None);
        }
        let next = script.remove(0);
        // A read that fails because the stream was stopped has the flag set when it fails
        if matches!(&next, Err(why) if why.starts_with("stopped")) {
            if let Some(stop) = self.1.lock().unwrap().as_ref() {
                stop.store(true, Ordering::SeqCst);
            }
        }
        next
    }
}

#[derive(Default)]
struct FakeBackend {
    senders: Mutex<Vec<Sender>>,
    script: Script,
    refuse: Mutex<Option<String>>,
    stop: StopSlot,
    stalled: Arc<AtomicBool>,
}

impl Backend for FakeBackend {
    fn senders(&self) -> Vec<Sender> {
        self.senders.lock().unwrap().clone()
    }
    fn open(&self, _key: &str) -> Result<Box<dyn Reader>, String> {
        match self.refuse.lock().unwrap().clone() {
            Some(why) => Err(why),
            None => Ok(Box::new(FakeReader(self.script.clone(), self.stop.clone(), self.stalled.clone()))),
        }
    }
}

struct Env {
    plugin: SpoutPlugin,
    host: FakeHost,
    backend: Arc<FakeBackend>,
}

fn env_with(senders: Vec<Sender>) -> Env {
    let backend = Arc::new(FakeBackend { senders: Mutex::new(senders), ..FakeBackend::default() });
    let mut plugin = SpoutPlugin::new(Some(backend.clone()));
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    let mut env = Env { plugin, host, backend };
    scan(&mut env);
    env
}

fn env() -> Env {
    env_with(vec![cam()])
}

fn pick(env: &mut Env, key: Option<&str>) {
    let source_id = key.map(|k| format!("{SOURCE_PREFIX}{k}")).unwrap_or_default();
    env.plugin.on_event(&mut env.host, &Event::SourceSelected { source_id });
}

fn stream(env: &mut Env, key: &str) {
    env.host.streaming = true;
    env.host.focused = Some(format!("{SOURCE_PREFIX}{key}"));
}

fn status(env: &Env) -> String {
    env.plugin.props(env.host.streaming)["status-text"].as_str().unwrap().to_string()
}

/// Waits for the scanning thread's listing to be posted, up to a few seconds.
fn wait_for_scan(env: &Env) {
    let end = Instant::now() + Duration::from_secs(5);
    while !env.host.posted.lock().unwrap().iter().any(|(token, _)| token == P_SCANNED) {
        assert!(Instant::now() < end, "the scan never reported");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// One look at the senders as the app does it: the timer fires, the thread looks, its posts are delivered.
fn scan(env: &mut Env) {
    env.plugin.on_event(&mut env.host, &Event::TimerFired { owner: NAME.into(), token: T_SCAN.into() });
    wait_for_scan(env);
    deliver(env);
}

fn deliver(env: &mut Env) {
    for (token, data) in env.host.take_posted() {
        env.plugin.on_event(&mut env.host, &Event::Posted { owner: NAME.into(), token, data });
    }
}

#[test]
fn senders_are_listed_by_name() {
    let env = env();
    assert!(env.host.calls.contains(&"add_stream_source spout:Cam".to_string()), "{:?}", env.host.calls);
    assert_eq!(env.plugin.sources["Cam"].name(), "Cam");
    assert_eq!(env.plugin.sources["Cam"].url(), SOURCE_PREFIX);
}

#[test]
fn a_sender_that_starts_joins_the_picker_and_one_that_stops_leaves() {
    let mut env = env();
    env.backend.senders.lock().unwrap().push(Sender { key: "OBS".into(), name: "OBS".into(), width: 2, height: 2 });
    scan(&mut env);
    assert!(env.plugin.sources.contains_key("OBS"));
    env.backend.senders.lock().unwrap().retain(|s| s.key != "OBS");
    scan(&mut env);
    assert!(!env.plugin.sources.contains_key("OBS"));
    assert!(env.host.calls.contains(&"remove_stream_source spout:OBS".to_string()), "{:?}", env.host.calls);
}

#[test]
fn a_streaming_sender_that_stops_waits_for_it_and_carries_on() {
    let mut env = env();
    stream(&mut env, "Cam");
    pick(&mut env, Some("Cam"));
    env.backend.senders.lock().unwrap().clear();
    scan(&mut env);
    assert!(env.plugin.sources.contains_key("Cam"));
    assert_eq!(status(&env), "Sender not running");
    let src = env.plugin.sources["Cam"].clone();
    assert!(src.open_feed().is_err());
    assert!(src.waiting_text().contains("isn't running"), "{}", src.waiting_text());
    env.backend.senders.lock().unwrap().push(cam());
    scan(&mut env);
    assert!(!env.plugin.states.contains_key("Cam"));
    assert!(env.host.calls.iter().filter(|c| *c == "add_stream_source spout:Cam").count() == 1, "it keeps its source");
}

#[test]
fn a_sender_that_stopped_leaves_once_its_stream_stops() {
    let mut env = env();
    stream(&mut env, "Cam");
    env.backend.senders.lock().unwrap().clear();
    scan(&mut env);
    env.host.streaming = false;
    env.plugin.on_event(&mut env.host, &Event::StreamsChanged { count: 0 });
    assert!(!env.plugin.sources.contains_key("Cam"));
}

#[test]
fn a_picked_sender_that_stops_stays_picked_until_another_source_is() {
    let mut env = env();
    pick(&mut env, Some("Cam"));
    env.backend.senders.lock().unwrap().clear();
    scan(&mut env);
    assert!(env.plugin.sources.contains_key("Cam"), "the picker keeps it");
    assert_eq!(status(&env), "Sender not running");
    env.backend.senders.lock().unwrap().push(cam());
    scan(&mut env);
    assert!(env.host.calls.iter().filter(|c| *c == "add_stream_source spout:Cam").count() == 1, "it keeps its source");
    env.backend.senders.lock().unwrap().clear();
    scan(&mut env);
    pick(&mut env, Some("other"));
    assert!(!env.plugin.sources.contains_key("Cam"));
    assert!(env.host.calls.contains(&"remove_stream_source spout:Cam".to_string()), "{:?}", env.host.calls);
}

#[test]
fn start_on_a_sender_that_stopped_since_it_was_picked_says_so() {
    let mut env = env();
    let src = env.plugin.sources["Cam"].clone();
    assert!(src.prepare(true));
    env.plugin.shared.senders.lock().unwrap().clear();
    assert!(!src.prepare(true));
    let posted = env.host.take_posted();
    assert!(posted.iter().any(|(t, _)| t == P_GONE), "{posted:?}");
    env.plugin.on_event(&mut env.host, &Event::Posted { owner: NAME.into(), token: P_GONE.into(), data: json!("Cam") });
    assert!(env.host.calls.iter().any(|c| c == "show_issue start Cam isn't running"), "{:?}", env.host.calls);
}

#[test]
fn a_system_with_no_backend_lists_nothing() {
    let mut plugin = SpoutPlugin::new(None);
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: T_SCAN.into() });
    assert!(plugin.scanner.is_none() && !plugin.scanning, "with nothing to read there is nothing to scan with");
    assert!(plugin.sources.is_empty());
    assert!(!host.calls.iter().any(|c| c.starts_with("add_stream_source")));
}

#[test]
fn the_card_shows_only_while_a_sender_is_picked() {
    let mut env = env();
    assert_eq!(env.plugin.props(false)["visible"], false);
    pick(&mut env, Some("Cam"));
    assert_eq!(env.plugin.props(false)["visible"], true);
    assert_eq!(status(&env), "Not streaming");
    stream(&mut env, "Cam");
    env.plugin.on_event(&mut env.host, &Event::Posted { owner: NAME.into(), token: P_REPORTED.into(), data: json!(["Cam", "ok", "4×2"]) });
    assert_eq!(status(&env), "4×2");
    pick(&mut env, None);
    assert_eq!(env.plugin.props(false)["visible"], false);
}

#[test]
fn the_frame_rate_is_kept_per_sender_and_defaults_to_30() {
    let mut env = env();
    assert_eq!(env.plugin.sources["Cam"].fps(), 30);
    pick(&mut env, Some("Cam"));
    assert_eq!(env.plugin.fps_items(), (vec![60, 30, 24, 15], 1));
    env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "fps-picked".into(), args: vec![json!(0)] });
    assert_eq!(env.plugin.sources["Cam"].fps(), 60);
    assert!(env.host.calls.contains(&"schedule_save".to_string()), "{:?}", env.host.calls);
    assert_eq!(env.plugin.get_config(), json!({"senders": {"Cam": {"fps": 60}}}));
    let mut again = SpoutPlugin::new(None);
    again.set_config(&json!({"senders": {"Cam": {"fps": 60}, "Bad": {"fps": 0}, "Worse": {"fps": "x"}, "Odd": 3}})).unwrap();
    assert_eq!(again.get_config(), json!({"senders": {"Cam": {"fps": 60}}}));
}

#[test]
fn a_rate_saved_from_elsewhere_is_still_offered() {
    let mut env = env();
    env.plugin.set_config(&json!({"senders": {"Cam": {"fps": 48}}})).unwrap();
    pick(&mut env, Some("Cam"));
    assert_eq!(env.plugin.fps_items(), (vec![60, 48, 30, 24, 15], 1));
}

fn open(env: &Env) -> Box<dyn VideoFeed> {
    env.plugin.sources["Cam"].open_feed().unwrap().1
}

#[test]
fn opening_waits_for_the_first_picture_and_hands_it_out_first() {
    let env = env();
    env.backend.script.lock().unwrap().push(Ok(Some(picture(4, 2, "RGBA", [255, 0, 0, 255]))));
    let mut feed = open(&env);
    let packet = feed.read().unwrap();
    assert_eq!(&packet.data[..12], &[4, 0, 0, 0, 2, 0, 0, 0, b'R', b'G', b'B', b'A']);
    assert_eq!(packet.data.len(), 12 + 4 * 2 * 4);
}

#[test]
fn a_sender_that_wont_open_says_why() {
    let env = env();
    *env.backend.refuse.lock().unwrap() = Some("format 10".into());
    let err = env.plugin.sources["Cam"].open_feed().err().unwrap();
    assert!(err.contains("can't read"), "{err}");
    assert!(env.plugin.sources["Cam"].waiting_text().contains("can't read"));
}

#[test]
fn a_sender_that_goes_while_streaming_ends_the_feed_and_waits() {
    let mut env = env();
    {
        let mut script = env.backend.script.lock().unwrap();
        script.push(Ok(Some(picture(4, 2, "BGRA", [0, 0, 255, 255]))));
        script.push(Err("the sender has stopped".into()));
    }
    let mut feed = open(&env);
    feed.read().unwrap();
    // Held back to the frame rate: the second read waits out the interval, then finds the sender gone
    let end = feed.read().unwrap_err();
    assert_eq!(end, FeedEnd::Error("the sender has stopped".into()));
    assert!(env.plugin.sources["Cam"].waiting_text().contains("isn't running"));
    deliver(&mut env);
    assert_eq!(env.plugin.states["Cam"].0, "gone");
}

#[test]
fn pictures_come_no_faster_than_the_frame_rate() {
    let env = env();
    env.plugin.shared.fps.lock().unwrap().insert("Cam".into(), 15);
    {
        let mut script = env.backend.script.lock().unwrap();
        for _ in 0..3 {
            script.push(Ok(Some(picture(4, 2, "RGBA", [1, 2, 3, 4]))));
        }
    }
    let mut feed = open(&env);
    let start = Instant::now();
    for _ in 0..3 {
        feed.read().unwrap();
    }
    // The first is free, the next two each wait a 15 fps interval
    assert!(start.elapsed() >= Duration::from_millis(120), "{:?}", start.elapsed());
}

#[test]
fn an_idle_sender_gets_its_last_picture_sent_again() {
    let env = env();
    env.backend.script.lock().unwrap().push(Ok(Some(picture(4, 2, "RGBA", [9, 9, 9, 9]))));
    let mut feed = open(&env);
    let first = feed.read().unwrap();
    let again = feed.read().unwrap();
    assert_eq!(first.data, again.data);
}

#[test]
fn a_new_size_is_reported_and_packets_carry_it() {
    let mut env = env();
    {
        let mut script = env.backend.script.lock().unwrap();
        script.push(Ok(Some(picture(4, 2, "RGBA", [0; 4]))));
        script.push(Ok(Some(picture(6, 4, "RGBA", [0; 4]))));
    }
    let mut feed = open(&env);
    feed.read().unwrap();
    env.host.take_posted();
    let second = feed.read().unwrap();
    assert_eq!(&second.data[..8], &[6, 0, 0, 0, 4, 0, 0, 0]);
    deliver(&mut env);
    assert_eq!(env.plugin.states["Cam"], ("ok".to_string(), "6×4".to_string()));
}

#[test]
fn a_reader_that_gets_stuck_says_so_in_the_row_and_says_ok_again_when_it_recovers() {
    let mut env = env();
    pick(&mut env, Some("Cam"));
    stream(&mut env, "Cam");
    env.backend.script.lock().unwrap().push(Ok(Some(picture(2, 2, "RGBA", [0; 4]))));
    let mut feed = open(&env);
    feed.read().unwrap();
    env.backend.stalled.store(true, Ordering::SeqCst);
    feed.read().unwrap();
    deliver(&mut env);
    assert_eq!(env.plugin.states["Cam"].0, "stalled");
    assert_eq!(status(&env), "Picture frozen");
    // The reader stops being stuck, but the row waits for a new picture
    env.backend.stalled.store(false, Ordering::SeqCst);
    feed.read().unwrap();
    deliver(&mut env);
    assert_eq!(env.plugin.states["Cam"].0, "stalled");
    env.backend.script.lock().unwrap().push(Ok(Some(picture(2, 2, "RGBA", [1; 4]))));
    feed.read().unwrap();
    deliver(&mut env);
    assert_eq!(env.plugin.states["Cam"], ("ok".to_string(), "2×2".to_string()));
}

#[test]
fn a_feed_that_was_stopped_leaves_the_row_alone_when_its_reader_turns_out_to_be_stuck() {
    let mut env = env();
    pick(&mut env, Some("Cam"));
    stream(&mut env, "Cam");
    env.backend.script.lock().unwrap().push(Ok(Some(picture(2, 2, "RGBA", [0; 4]))));
    let mut feed = open(&env);
    feed.read().unwrap();
    feed.stopper().stop();
    env.backend.stalled.store(true, Ordering::SeqCst);
    assert_eq!(feed.read().unwrap_err(), FeedEnd::Stopped);
    deliver(&mut env);
    assert_ne!(env.plugin.states["Cam"].0, "stalled");
}

#[test]
fn stopping_ends_the_feed() {
    let env = env();
    env.backend.script.lock().unwrap().push(Ok(Some(picture(2, 2, "RGBA", [0; 4]))));
    let mut feed = open(&env);
    feed.read().unwrap();
    feed.stopper().stop();
    assert_eq!(feed.read().unwrap_err(), FeedEnd::Stopped);
}

fn decode(p: &Picture) -> Frame {
    let packet = Packet { data: packet_of(p), time_ns: 0 };
    SpoutDecoder.decode(&packet).newest.expect("a frame")
}

#[test]
fn every_byte_order_decodes_to_the_same_colours_with_alpha_dropped() {
    let red_rgba = decode(&picture(4, 2, "RGBA", [255, 0, 0, 255]));
    assert_eq!(red_rgba.format, PixelFormat::I420);
    assert_eq!((red_rgba.width, red_rgba.height), (4, 2));
    // Red: low luma, V (red difference) at its top
    let (y, _, v) = red_rgba.i420_planes().unwrap();
    assert!(y[0] < 100 && v[0] > 200, "{} {}", y[0], v[0]);
    for (fourcc, pixel) in [("RGBx", [255, 0, 0, 0]), ("BGRA", [0, 0, 255, 255]), ("BGRx", [0, 0, 255, 7])] {
        assert_eq!(decode(&picture(4, 2, fourcc, pixel)).data, red_rgba.data, "{fourcc}");
    }
    // A see-through pixel counts as solid
    assert_eq!(decode(&picture(4, 2, "RGBA", [255, 0, 0, 0])).data, red_rgba.data);
}

#[test]
fn a_broken_packet_gives_no_frame() {
    let mut decoder = SpoutDecoder;
    let mut good = packet_of(&picture(2, 2, "RGBA", [0; 4]));
    good.pop();
    for data in [Vec::new(), vec![0; 11], good, packet_of(&picture(2, 2, "XXXX", [0; 4]))] {
        assert!(decoder.decode(&Packet { data, time_ns: 0 }).newest.is_none());
    }
}

/// A backend whose look at the senders waits until the test lets it go.
struct SlowBackend {
    gate: Mutex<mpsc::Receiver<()>>,
    looks: AtomicU32,
    senders: Vec<Sender>,
}

impl Backend for SlowBackend {
    fn senders(&self) -> Vec<Sender> {
        self.looks.fetch_add(1, Ordering::SeqCst);
        let _ = self.gate.lock().unwrap().recv_timeout(Duration::from_secs(10));
        self.senders.clone()
    }
    fn open(&self, _key: &str) -> Result<Box<dyn Reader>, String> {
        Err("not for this test".into())
    }
}

fn slow() -> (Arc<SlowBackend>, mpsc::Sender<()>, SpoutPlugin, FakeHost) {
    let (release, gate) = mpsc::channel();
    let backend = Arc::new(SlowBackend { gate: Mutex::new(gate), looks: AtomicU32::new(0), senders: vec![cam()] });
    let mut plugin = SpoutPlugin::new(Some(backend.clone()));
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    (backend, release, plugin, host)
}

fn tick(plugin: &mut SpoutPlugin, host: &mut FakeHost) {
    plugin.on_event(host, &Event::TimerFired { owner: NAME.into(), token: T_SCAN.into() });
}

fn looks_reach(backend: &SlowBackend, count: u32) {
    let end = Instant::now() + Duration::from_secs(5);
    while backend.looks.load(Ordering::SeqCst) < count {
        assert!(Instant::now() < end, "the backend was never asked ({count})");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_slow_look_at_the_senders_never_holds_up_the_timer_hook() {
    let (backend, release, mut plugin, mut host) = slow();
    let started = Instant::now();
    tick(&mut plugin, &mut host);
    looks_reach(&backend, 1);
    tick(&mut plugin, &mut host);
    tick(&mut plugin, &mut host);
    assert!(started.elapsed() < Duration::from_secs(2), "the hook waited on the backend ({:?})", started.elapsed());
    assert!(host.is_scheduled(T_SCAN), "the next look stays scheduled");
    // Still waiting on the first look: the other ticks started no second one
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(backend.looks.load(Ordering::SeqCst), 1);
    assert!(plugin.sources.is_empty());
    release.send(()).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let posted = loop {
        let posted = host.take_posted();
        if !posted.is_empty() {
            break posted;
        }
        assert!(Instant::now() < end, "the listing never came back");
        std::thread::sleep(Duration::from_millis(2));
    };
    for (token, data) in posted {
        plugin.on_event(&mut host, &Event::Posted { owner: NAME.into(), token, data });
    }
    assert!(plugin.sources.contains_key("Cam"), "the listing is applied once it arrives");
    // With the look done, the next tick starts the next
    tick(&mut plugin, &mut host);
    looks_reach(&backend, 2);
}

#[test]
fn a_plugin_that_is_switched_off_or_dropped_stops_its_scanning_thread() {
    let (backend, release, mut plugin, mut host) = slow();
    tick(&mut plugin, &mut host);
    looks_reach(&backend, 1);
    plugin.shutdown(&mut host);
    assert!(plugin.scanner.is_none());
    release.send(()).unwrap();
    // The look that was under way ends, and its listing is not applied
    let end = Instant::now() + Duration::from_secs(5);
    while host.posted.lock().unwrap().is_empty() {
        assert!(Instant::now() < end, "the look never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
    for (token, data) in host.take_posted() {
        plugin.on_event(&mut host, &Event::Posted { owner: NAME.into(), token, data });
    }
    assert!(plugin.sources.is_empty());
    tick(&mut plugin, &mut host);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(backend.looks.load(Ordering::SeqCst), 1, "no look starts after switching off");
    // The thread held a copy of the backend and lets it go when it ends
    drop(plugin);
    let end = Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(&backend) > 1 {
        assert!(Instant::now() < end, "the scanning thread is still running");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn what_the_backend_says_is_wrong_shows_in_the_diagnostics() {
    struct Down;
    impl Backend for Down {
        fn senders(&self) -> Vec<Sender> {
            Vec::new()
        }
        fn open(&self, _key: &str) -> Result<Box<dyn Reader>, String> {
            Err("down".into())
        }
        fn problem(&self) -> Option<String> {
            Some("PipeWire isn't running".into())
        }
    }
    let mut plugin = SpoutPlugin::new(Some(Arc::new(Down)));
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    assert_eq!(plugin.diagnostics()[0].1, "0 found");
    let mut env = Env { plugin, host, backend: Arc::default() };
    scan(&mut env);
    assert_eq!(env.plugin.diagnostics()[0].1, "PipeWire isn't running");
}

/// BGRA pixels that run through every channel value, for conversion checks.
fn colourful(width: u32, height: u32) -> Picture {
    let data = (0..width * height).flat_map(|i| [(i * 7 + 3) as u8, (i * 13) as u8, (i * 29 + 100) as u8, 255]).collect();
    Picture { width, height, fourcc: "BGRA", data }
}

/// The same picture as the card hands it out: the planes the CPU conversion makes of it.
fn planes_of(picture: &Picture) -> Picture {
    let frame = decode(picture);
    Picture { width: picture.width, height: picture.height, fourcc: I420, data: frame.data.to_vec() }
}

#[test]
fn a_picture_that_is_planes_already_is_passed_on_as_it_is() {
    for (w, h) in [(1, 1), (2, 2), (3, 5), (4, 7), (17, 9), (64, 48)] {
        let planar = planes_of(&colourful(w, h));
        assert_eq!(planar.data.len(), crate::spout_shared::planes::i420_len(w, h), "{w}x{h}");
        let frame = decode(&planar);
        assert_eq!((frame.width, frame.height, frame.format), (w, h, PixelFormat::I420));
        assert_eq!(frame.data.to_vec(), planar.data, "{w}x{h}");
    }
}

#[test]
fn the_packet_of_planes_carries_its_fourcc_and_round_trips() {
    let planar = planes_of(&colourful(5, 3));
    let packet = packet_of(&planar);
    assert_eq!(&packet[..12], &[5, 0, 0, 0, 3, 0, 0, 0, b'I', b'4', b'2', b'0']);
    assert_eq!(&packet[12..], &planar.data[..]);
}

#[test]
fn a_broken_packet_of_planes_gives_no_frame() {
    let mut decoder = SpoutDecoder;
    let good = packet_of(&planes_of(&colourful(5, 3)));
    let mut short = good.clone();
    short.pop();
    let mut long = good.clone();
    long.push(0);
    // Planes the size of BGRA pixels, and a picture with no size
    let bgra_sized = packet_of(&Picture { width: 5, height: 3, fourcc: I420, data: vec![0; 5 * 3 * 4] });
    let no_size = packet_of(&Picture { width: 0, height: 3, fourcc: I420, data: Vec::new() });
    for data in [short, long, bgra_sized, no_size] {
        assert!(decoder.decode(&Packet { data, time_ns: 0 }).newest.is_none());
    }
    assert!(decoder.decode(&Packet { data: good, time_ns: 0 }).newest.is_some());
}

#[test]
fn a_feed_carries_planes_and_sends_them_again_when_the_sender_is_idle() {
    let env = env();
    let planar = planes_of(&colourful(5, 3));
    env.backend.script.lock().unwrap().push(Ok(Some(planar.clone())));
    let mut feed = open(&env);
    let mut decoder = SpoutDecoder;
    let first = feed.read().unwrap();
    assert_eq!(decoder.decode(&first).newest.unwrap().data.to_vec(), planar.data);
    let again = feed.read().unwrap();
    assert_eq!(again.data, first.data);
}

#[test]
fn a_stop_of_the_feed_sets_the_flag_its_reader_waits_on_the_card_with() {
    let env = env();
    env.backend.script.lock().unwrap().push(Ok(Some(picture(2, 2, "RGBA", [0; 4]))));
    let feed = open(&env);
    let flag = env.backend.stop.lock().unwrap().clone().expect("the reader was given a flag");
    assert!(!flag.load(Ordering::SeqCst));
    feed.stopper().stop();
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn a_stop_while_the_feed_is_opening_sets_the_flag_and_ends_the_open_quietly() {
    let env = env();
    let open_stop = OpenStop::default();
    // The sender gives nothing yet; the stop comes while the open is waiting for the first picture
    let stopper = open_stop.clone();
    let seen = env.backend.stop.clone();
    let watcher = std::thread::spawn(move || {
        while seen.lock().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(1));
        }
        stopper.stop();
        seen.lock().unwrap().clone().unwrap()
    });
    let before = env.plugin.sources["Cam"].waiting_text();
    let err = env.plugin.sources["Cam"].open_feed_stoppable(&open_stop).err().unwrap();
    assert_eq!(err, "stopped");
    assert!(watcher.join().unwrap().load(Ordering::SeqCst));
    assert_eq!(env.plugin.sources["Cam"].waiting_text(), before, "a stop is not a sender that went");
}

#[test]
fn a_read_that_fails_because_of_a_stop_ends_as_stopped_not_as_a_sender_that_went() {
    let mut env = env();
    {
        let mut script = env.backend.script.lock().unwrap();
        script.push(Ok(Some(picture(2, 2, "RGBA", [0; 4]))));
        script.push(Err("stopped while the card was still working".into()));
    }
    let mut feed = open(&env);
    feed.read().unwrap();
    assert_eq!(feed.read().unwrap_err(), FeedEnd::Stopped);
    deliver(&mut env);
    assert_ne!(env.plugin.states.get("Cam").map(|s| s.0.as_str()), Some("gone"));
}

#[test]
fn a_preset_brings_back_the_picked_senders_frame_rate_and_nothing_else() {
    let mut env = env();
    assert!(env.plugin.info().in_presets);
    pick(&mut env, Some("Cam"));
    env.plugin.apply_preset(&mut env.host, &json!({"senders": {"Cam": {"fps": 60}, "Other": {"fps": 15}}})).unwrap();
    assert_eq!(env.plugin.shared.fps("Cam"), 60);
    assert_eq!(env.plugin.shared.fps("Other"), DEFAULT_FPS, "a sender that isn't picked keeps its own rate");
    // An older preset, or a rate out of range, changes nothing
    env.plugin.apply_preset(&mut env.host, &json!({})).unwrap();
    env.plugin.apply_preset(&mut env.host, &json!({"senders": {"Cam": {"fps": 9999}}})).unwrap();
    assert_eq!(env.plugin.shared.fps("Cam"), 60);
}

#[test]
fn a_preset_saved_at_the_default_rate_can_bring_it_back_and_a_sender_that_is_away_keeps_the_rate_without_a_restart() {
    let mut env = env();
    pick(&mut env, Some("Cam"));
    let saved = env.plugin.get_config();
    assert_eq!(saved["senders"]["Cam"]["fps"], DEFAULT_FPS);
    env.plugin.apply_preset(&mut env.host, &json!({"senders": {"Cam": {"fps": 60}}})).unwrap();
    env.plugin.apply_preset(&mut env.host, &saved).unwrap();
    assert_eq!(env.plugin.shared.fps("Cam"), DEFAULT_FPS);
    // The sender goes away while it streams
    env.backend.senders.lock().unwrap().clear();
    scan(&mut env);
    stream(&mut env, "Cam");
    env.host.calls.clear();
    env.plugin.apply_preset(&mut env.host, &json!({"senders": {"Cam": {"fps": 15}}})).unwrap();
    assert!(!env.host.calls.contains(&"reconnect_stream".to_string()), "{:?}", env.host.calls);
    assert_eq!(env.plugin.shared.fps("Cam"), 15);
}
