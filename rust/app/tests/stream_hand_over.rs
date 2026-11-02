// Moving a running stream onto another route, USB when a cable comes, without a gap (PhoneSource.hand_over).
mod rig;

use rig::Rig;
use std::sync::atomic::Ordering;
use telescope_core::host::HostStreams;
use telescope_core::source::RouteSpec;

const WIFI: &str = "https://pixel:8080/v1/video.h264";
const USB: &str = "https://usb-SER:8080/v1/video";

fn usb() -> RouteSpec {
    RouteSpec::Usb { serial: "SER".into() }
}

fn on_wifi(name: &str) -> Rig {
    let r = Rig::new(name);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.forget_heard();
    *r.link.prepared.lock().unwrap() = Some(USB.into());
    r
}

/// Asks as Connection's USB switch does.
fn hand_over(r: &Rig) -> bool {
    let at = r.w.state.borrow().index("panel").unwrap();
    let asked = r.w.state.borrow_mut().call(at, |_, host| host.hand_over_stream(&usb(), Some("pixel"))).unwrap();
    r.w.process();
    r.pump();
    asked
}

/// The worker opens the new route, as it does before it says how it went.
fn opens_the_new_route(r: &Rig) {
    let opener = r.worker(0).handed.lock().unwrap().clone().unwrap();
    assert!(opener(&Default::default()).is_err(), "the fake has no video, but the session is open");
}

fn url(r: &Rig) -> String {
    r.source("pixel").session().unwrap().url
}

#[test]
fn a_hand_over_moves_the_stream_and_tells_the_plugins_where_it_is_now() {
    let r = on_wifi("hand-over");
    assert!(hand_over(&r));
    assert!(r.worker(0).did(&format!("hand_over {USB}")));
    assert_eq!(url(&r), WIFI, "still Wi-Fi until it's moved");
    assert!(!hand_over(&r), "one at a time");
    opens_the_new_route(&r);
    let control = r.link.control("pixel");
    assert!(control.retargeted.lock().unwrap().is_empty(), "only once the new route proved itself");
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Moved(USB.into()));
    r.pump();
    assert_eq!(r.link.logged("commit pixel"), 1);
    assert_eq!(url(&r), USB);
    assert_eq!(*control.retargeted.lock().unwrap(), [USB]);
    assert!(!control.closed.load(Ordering::SeqCst));
    assert!(r.worker(0).did("set_opener"));
    let session = r.session_id("pixel");
    let heard = r.heard();
    let last = heard.iter().rposition(|l| l.starts_with("microphone url ")).unwrap();
    assert_eq!(heard[last], format!("microphone url {USB} session {session}"), "the mic follows it there");
    r.until("the old route's session goes", |r| r.link.released.lock().unwrap().first().is_some_and(|f| f.load(Ordering::SeqCst)));
    assert!(hand_over(&r), "it can go on to another");
}

#[test]
fn a_failed_hand_over_leaves_the_stream_where_it_was() {
    let r = on_wifi("hand-over-fails");
    assert!(hand_over(&r));
    opens_the_new_route(&r);
    r.worker(0).event(telescope_shell::worker::WorkerEvent::MoveFailed(USB.into()));
    r.pump();
    assert_eq!(r.link.logged("drop_route pixel true"), 1);
    assert_eq!(r.link.logged("commit pixel"), 0);
    assert_eq!(url(&r), WIFI);
    assert!(r.link.control("pixel").retargeted.lock().unwrap().is_empty());
    assert!(r.heard().is_empty(), "plugins hear nothing of it: {:?}", r.heard());
    assert!(hand_over(&r), "it can try again");
}

#[test]
fn a_worker_that_cant_hand_over_isnt_asked_and_the_link_keeps_no_route() {
    let r = on_wifi("hand-over-cant");
    r.worker(0).refuses_hand_over.store(true, Ordering::SeqCst);
    r.w.with_flow(|_, _| ()); // the state plugins read, as the flow has it
    assert!(!hand_over(&r));
    assert_eq!(r.link.logged("prepare pixel"), 0);
    assert_eq!(url(&r), WIFI);
}

#[test]
fn a_worker_that_turns_it_down_lets_go_of_the_route_at_once() {
    let r = on_wifi("hand-over-declined");
    r.worker(0).declines_hand_over.store(true, Ordering::SeqCst);
    hand_over(&r);
    assert!(r.worker(0).did(&format!("hand_over {USB}")));
    assert_eq!(r.link.logged("drop_route pixel false"), 1);
    assert_eq!(url(&r), WIFI);
}

#[test]
fn no_route_ready_means_no_hand_over() {
    let r = on_wifi("hand-over-no-route");
    *r.link.prepared.lock().unwrap() = None;
    assert!(!hand_over(&r));
    assert!(!r.worker(0).did(&format!("hand_over {USB}")));
}

#[test]
fn a_stream_thats_recovering_isnt_asked_to_move() {
    let r = on_wifi("hand-over-recovering");
    r.worker(0).says("reconnecting", "Connection lost");
    r.pump();
    assert!(!hand_over(&r));
    assert_eq!(r.link.logged("prepare pixel"), 0);
}
