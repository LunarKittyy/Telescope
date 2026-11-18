use super::worker::tests::{picture, wait_for, Log, Mock};
use super::*;
use std::sync::atomic::Ordering;
use telescope_core::testing::FakeHost;

struct Ctx(String);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        &self.0
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn plugin() -> (SpoutSendPlugin, FakeHost, Arc<Log>) {
    let log = Arc::new(Log::default());
    let mut p = SpoutSendPlugin::new(Some(Arc::new(Mock(log.clone()))));
    let mut host = FakeHost::new();
    p.setup(&mut host);
    p.on_focus_gained(&mut host, &StreamInfo { source_id: "phone-a".into(), url: "u".into(), session: 1 });
    (p, host, log)
}

fn toggle(p: &mut SpoutSendPlugin, host: &mut FakeHost, on: bool) {
    p.on_ui(host, &UiEvent { ui: "card".into(), callback: "toggled".into(), args: vec![json!(on)] });
}

fn step(p: &mut SpoutSendPlugin) -> Box<dyn CpuStep> {
    match p.frame_step() {
        Some(FrameStep::Cpu(step)) => step,
        _ => panic!("no step"),
    }
}

#[test]
fn a_sender_is_named_after_the_stream_and_made_unique() {
    assert_eq!(sender_name("Pixel 7", &[]), "Telescope - Pixel 7");
    assert_eq!(sender_name("Pixel 7", &["Telescope - Pixel 7"]), "Telescope - Pixel 7 (2)");
    assert_eq!(sender_name("Pixel 7", &["Telescope - Pixel 7", "Telescope - Pixel 7 (2)"]), "Telescope - Pixel 7 (3)");
    assert_eq!(sender_name("  ", &[]), "Telescope");
    assert_eq!(sender_name("Pi\nxel", &[]), "Telescope - Pixel");
}

#[test]
fn a_long_name_is_cut_at_a_character_and_keeps_room_for_the_number() {
    let long = "é".repeat(300);
    let first = sender_name(&long, &[]);
    assert!(first.len() <= NAME_BYTES && first.starts_with("Telescope - é"));
    let second = sender_name(&long, &[first.as_str()]);
    assert!(second.len() <= NAME_BYTES && second.ends_with(" (2)") && second != first);
}

#[test]
fn nothing_is_shared_until_a_stream_is_switched_on_and_the_step_is_not_there_either() {
    let (mut p, mut host, _) = plugin();
    assert!(p.frame_step().is_none());
    assert_eq!(p.get_config(), json!({"streams": {}}));
    toggle(&mut p, &mut host, true);
    assert!(p.frame_step().is_some());
    assert!(p.get_config()["streams"]["phone-a"].is_object());
    toggle(&mut p, &mut host, false);
    assert!(p.frame_step().is_none());
}

#[test]
fn the_step_hands_the_pictures_of_a_shared_stream_to_a_sender_and_skips_the_others() {
    let (mut p, mut host, log) = plugin();
    toggle(&mut p, &mut host, true);
    let mut tap = step(&mut p);
    assert_eq!(tap.touch(&Ctx("phone-a".into())), Touch::Reads);
    assert_eq!(tap.touch(&Ctx("phone-b".into())), Touch::Nothing);
    let mut frame = picture(2, 2, 5);
    tap.process(&mut frame, &Ctx("phone-a".into()));
    tap.process(&mut frame, &Ctx("phone-b".into()));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    assert_eq!(log.opened.lock().unwrap().len(), 1, "only the shared stream has a sender");
    assert_eq!(frame.data[0], 5, "the picture is left as it is");
    assert!(matches!(p.shared.state("phone-a"), State::Sending(2, 2)));
}

#[test]
fn switching_a_stream_off_takes_its_sender_down() {
    let (mut p, mut host, log) = plugin();
    toggle(&mut p, &mut host, true);
    let mut tap = step(&mut p);
    tap.process(&mut picture(2, 2, 1), &Ctx("phone-a".into()));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    toggle(&mut p, &mut host, false);
    assert_eq!(log.closed.load(Ordering::SeqCst), 1);
    tap.process(&mut picture(2, 2, 1), &Ctx("phone-a".into()));
    assert_eq!(log.opened.lock().unwrap().len(), 1, "a step that was already out there makes no new sender");
}

#[test]
fn a_stream_that_stops_sending_lets_its_sender_go_and_a_new_picture_makes_another() {
    let (mut p, mut host, log) = plugin();
    toggle(&mut p, &mut host, true);
    let mut shared = Shared::new(Some(Arc::new(Mock(log.clone()))));
    Arc::get_mut(&mut shared).unwrap().idle = Duration::from_millis(50);
    p.shared = shared;
    toggle(&mut p, &mut host, true);
    let mut tap = step(&mut p);
    tap.process(&mut picture(2, 2, 1), &Ctx("phone-a".into()));
    wait_for("the sender to go", || log.closed.load(Ordering::SeqCst) == 1);
    tap.process(&mut picture(2, 2, 2), &Ctx("phone-a".into()));
    wait_for("another sender", || log.opened.lock().unwrap().len() == 2);
}

#[test]
fn the_settings_come_back_and_name_the_streams_again() {
    let (mut p, mut host, _) = plugin();
    toggle(&mut p, &mut host, true);
    let saved = p.get_config();
    let (mut q, _, _) = plugin();
    q.set_config(&saved).unwrap();
    assert_eq!(q.get_config(), saved);
    assert!(q.frame_step().is_some());
    q.set_config(&json!({})).unwrap();
    assert!(q.frame_step().is_none());
}

#[test]
fn the_card_says_what_the_shown_stream_is_doing() {
    let (mut p, mut host, log) = plugin();
    assert_eq!(p.props()["checked"], json!(false));
    toggle(&mut p, &mut host, true);
    assert_eq!(p.props()["checked"], json!(true));
    assert!(p.props()["status-text"].as_str().unwrap().contains("Telescope"), "{}", p.props());
    let mut tap = step(&mut p);
    tap.process(&mut picture(4, 2, 1), &Ctx("phone-a".into()));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    wait_for("the state", || p.props()["status-kind"] == json!("status_ok"));
    assert!(p.props()["status-text"].as_str().unwrap().contains("4 × 2"));
}

#[test]
fn without_a_way_to_share_the_switch_is_off_and_says_so() {
    let mut p = SpoutSendPlugin::new(None);
    let mut host = FakeHost::new();
    p.on_focus_gained(&mut host, &StreamInfo { source_id: "phone-a".into(), url: "u".into(), session: 1 });
    assert_eq!(p.props()["enabled"], json!(false));
    assert_eq!(p.props()["status-kind"], json!("status_warn"));
}

#[test]
fn a_thread_that_ends_after_its_stream_was_switched_off_leaves_no_state_behind() {
    let (mut p, mut host, log) = plugin();
    toggle(&mut p, &mut host, true);
    let mut tap = step(&mut p);
    tap.process(&mut picture(2, 2, 1), &Ctx("phone-a".into()));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    toggle(&mut p, &mut host, false);
    toggle(&mut p, &mut host, true);
    std::thread::sleep(Duration::from_millis(100));
    assert!(p.shared.states.lock().unwrap().is_empty(), "{:?}", p.shared.states.lock().unwrap());
    assert_eq!(p.shared.state("phone-a"), State::Idle);
}

fn cut_out_events(host: &FakeHost) -> Vec<(String, bool)> {
    host.events.iter().filter_map(|e| if let Event::CutOutWanted { source_id, on } = e { Some((source_id.clone(), *on)) } else { None }).collect()
}

fn toggle_transparent(p: &mut SpoutSendPlugin, host: &mut FakeHost, on: bool) {
    p.on_ui(host, &UiEvent { ui: "card".into(), callback: "transparent-toggled".into(), args: vec![json!(on)] });
}

#[test]
fn a_transparent_stream_asks_for_its_cut_out_and_stops_asking_when_it_goes() {
    let (mut p, mut host, _) = plugin();
    assert_eq!(p.props()["transparent-shown"], json!(false));
    toggle(&mut p, &mut host, true);
    assert_eq!(p.props()["transparent-shown"], json!(true));
    assert!(cut_out_events(&host).is_empty(), "opaque by default");
    toggle_transparent(&mut p, &mut host, true);
    assert_eq!(cut_out_events(&host), [("phone-a".to_string(), true)]);
    assert_eq!(p.props()["transparent"], json!(true));
    // Told again once the steps that cut out are up, and again when a source is shown
    p.on_plugins_ready(&mut host);
    p.on_event(&mut host, &Event::SourceShown { source_id: "phone-a".into() });
    assert_eq!(cut_out_events(&host).len(), 3);
    toggle_transparent(&mut p, &mut host, false);
    assert_eq!(cut_out_events(&host).last(), Some(&("phone-a".to_string(), false)));
    // Switching sharing off while transparent is a change too
    toggle_transparent(&mut p, &mut host, true);
    toggle(&mut p, &mut host, false);
    assert_eq!(cut_out_events(&host).last(), Some(&("phone-a".to_string(), false)));
}

#[test]
fn the_transparent_choice_is_saved_with_the_stream() {
    let (mut p, mut host, _) = plugin();
    toggle(&mut p, &mut host, true);
    toggle_transparent(&mut p, &mut host, true);
    let saved = p.get_config();
    assert_eq!(saved["streams"]["phone-a"]["transparent"], json!(true));
    let (mut q, mut host2, _) = plugin();
    q.set_config(&saved).unwrap();
    assert_eq!(q.get_config(), saved);
    q.on_plugins_ready(&mut host2);
    assert_eq!(cut_out_events(&host2), [("phone-a".to_string(), true)]);
}

#[test]
fn the_cut_out_goes_out_as_alpha_only_for_a_stream_that_asked() {
    use telescope_core::frame::Alpha;
    let (mut p, mut host, log) = plugin();
    toggle(&mut p, &mut host, true);
    let mut tap = step(&mut p);
    let cut = |frame: &mut Frame| assert!(frame.set_alpha(Arc::new(Alpha::new(2, 2, vec![0, 64, 128, 255], false))));
    let mut frame = picture(2, 2, 9);
    cut(&mut frame);
    tap.process(&mut frame, &Ctx("phone-a".into()));
    wait_for("the picture", || !log.sent.lock().unwrap().is_empty());
    assert!(log.sent.lock().unwrap()[0].as_chunks::<4>().0.iter().all(|px| px[3] == 255), "solid unless asked");
    toggle_transparent(&mut p, &mut host, true);
    let before = log.sent.lock().unwrap().len();
    cut(&mut frame);
    tap.process(&mut frame, &Ctx("phone-a".into()));
    wait_for("the second picture", || log.sent.lock().unwrap().len() > before);
    let sent = log.sent.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent.as_chunks::<4>().0.iter().map(|px| px[3]).collect::<Vec<_>>(), [0, 64, 128, 255]);
    assert_eq!(frame.alpha().map(|a| a.data.clone()), Some(vec![0, 64, 128, 255]), "the frame itself keeps its cut-out for the camera's flatten");
}

#[test]
fn switching_the_plugin_off_stops_asking_for_every_cut_out() {
    let (mut p, mut host, _) = plugin();
    toggle(&mut p, &mut host, true);
    toggle_transparent(&mut p, &mut host, true);
    assert_eq!(cut_out_events(&host), [("phone-a".to_string(), true)]);
    p.shutdown(&mut host);
    assert_eq!(cut_out_events(&host).last(), Some(&("phone-a".to_string(), false)));
    assert!(p.told.is_empty());
}
