//! The headset viewer: the page and its link, the WebSocket upgrade, the pictures a headset is sent and how few of them wait.

use super::*;
use std::io::Write;
use std::process::Command;
use std::sync::mpsc;
use telescope_core::frame::{PixelFormat, StreamFrame};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const TOKEN: &str = "abcdefghijklmnopqrstuvwx";
const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

struct Store {
    outgoing: Mutex<Option<Frame>>,
    epoch: AtomicU64,
}

impl Store {
    fn new() -> Store {
        Store { outgoing: Mutex::new(None), epoch: AtomicU64::new(0) }
    }
}

impl FrameStore for Store {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        Vec::new()
    }
    fn stream_frame(&self, _source_id: Option<&str>) -> Option<StreamFrame> {
        None
    }
    fn replace_frame(&self, _source_id: Option<&str>, _frame: Frame, _number: u64) -> bool {
        false
    }
    fn outgoing_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        let frame = self.outgoing.lock().unwrap().clone()?;
        Some(StreamFrame { source_id: source_id?.into(), name: String::new(), frame })
    }
    fn pipeline_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

/// A 64x36 picture of one colour.
fn shot(rgb: [u8; 3]) -> Frame {
    Frame::new(64, 36, PixelFormat::Rgba, [rgb[0], rgb[1], rgb[2], 255].repeat(64 * 36))
}

fn job(left: [u8; 3], right: [u8; 3], number: u64) -> Job {
    Job {
        target: "a".into(), left: shot(left), right: shot(right), tune: Tune::default(), number, handed: Arc::new(Vec::new()), epoch: 0, at: Instant::now(),
    }
}

fn decode(jpeg: &[u8]) -> (usize, usize, Vec<u8>) {
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB));
    let data = decoder.decode().expect("a JPEG");
    let (w, h) = decoder.dimensions().expect("a size");
    (w, h, data)
}

fn px(picture: &(usize, usize, Vec<u8>), x: usize, y: usize) -> [u8; 3] {
    let at = (y * picture.0 + x) * 3;
    [picture.2[at], picture.2[at + 1], picture.2[at + 2]]
}

/// Close to this colour, which is all a JPEG of a flat colour promises.
fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| (*g as i32 - w as i32).abs() < 40)
}

/// A headset's end of the connection: what the server writes arrives in `sent` (as the chunks it wrote), and what `typed` is given is what the
/// headset sent. Reads time out when nothing was sent, as a socket with a read timeout does.
struct Pipe {
    out: mpsc::SyncSender<Vec<u8>>,
    input: mpsc::Receiver<Vec<u8>>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.input.recv_timeout(Duration::from_millis(2)) {
            Ok(bytes) => {
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ErrorKind::TimedOut.into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(0),
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out.send(buf.to_vec()).map_err(|_| ErrorKind::BrokenPipe)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Socket for Pipe {
    fn set_read_timeout(&mut self, _timeout: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }
}

/// `bound` chunks can be written before a write waits for `sent` to be read: 0 for every write to wait.
fn pipe(bound: usize) -> (Box<dyn Socket>, mpsc::Receiver<Vec<u8>>, mpsc::Sender<Vec<u8>>) {
    let (out, sent) = mpsc::sync_channel(bound);
    let (typed, input) = mpsc::channel();
    (Box::new(Pipe { out, input }), sent, typed)
}

/// (opcode, payload) of the next message the headset got.
fn message(sent: &mpsc::Receiver<Vec<u8>>) -> (u8, Vec<u8>) {
    let wait = Duration::from_secs(10);
    let head = sent.recv_timeout(wait).expect("a message");
    let len = match head[1] {
        126 => u16::from_be_bytes([head[2], head[3]]) as usize,
        127 => u64::from_be_bytes(head[2..10].try_into().unwrap()) as usize,
        n => n as usize,
    };
    (head[0] & 0x0F, if len == 0 { Vec::new() } else { sent.recv_timeout(wait).expect("a payload") })
}

/// A browser's frame: masked, as they always are.
fn masked(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [1u8, 2, 3, 4];
    let mut out = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

fn ws_request(token: &str) -> Request {
    let header = |k: &str, v: &str| (k.to_string(), v.to_string());
    Request {
        method: "GET".into(),
        target: format!("/ws?token={token}"),
        headers: vec![header("Upgrade", "websocket"), header("Connection", "Upgrade"), header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="), header("Sec-WebSocket-Version", "13")],
    }
}

fn get(target: &str) -> Request {
    Request { method: "GET".into(), target: target.into(), headers: Vec::new() }
}

fn stats() -> ServerStats {
    ServerStats::new(7484, 7484, false, "3D headset")
}

fn open_feed() -> Arc<Feed> {
    let feed = Arc::new(Feed::new(TOKEN));
    feed.open();
    feed
}

// ── The page and its link ─────────────────────────────────────────────────

#[test]
fn the_accept_key_is_the_one_in_the_standard() {
    assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[test]
fn the_page_and_its_script_are_served_and_the_picture_needs_the_link() {
    let handler = Headset(open_feed());
    let stats = stats();
    let page = handler.handle(&get("/"), &stats);
    assert_eq!((page.status, page.content_type.as_str()), (200, "text/html; charset=utf-8"));
    assert!(String::from_utf8_lossy(&page.body).contains("Enter VR") && String::from_utf8_lossy(&page.body).contains("/headset.js"));
    assert_eq!(stats.count(Step::Pages), 1);
    let script = handler.handle(&get("/headset.js"), &stats);
    assert_eq!((script.status, script.body.as_slice()), (200, SCRIPT));
    assert_eq!(handler.handle(&get("/elsewhere"), &stats).status, 404);
    // no link, a wrong one and an old one are all turned away
    for target in ["/ws", "/ws?token=", "/ws?token=wrong", "/ws?token=abcdefghijklmnopqrstuvwy"] {
        assert_eq!(handler.handle(&get(target), &stats).status, 403, "{target}");
    }
    assert_eq!(stats.count(Step::Refused), 4);
    // the right link without an upgrade has nothing to give
    assert_eq!(handler.handle(&get(&format!("/ws?token={TOKEN}")), &stats).status, 503);
}

#[test]
fn only_a_headset_with_the_link_gets_the_upgrade_and_only_two_at_a_time() {
    let feed = open_feed();
    let handler = Headset(feed.clone());
    let stats = stats();
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats).expect("an upgrade");
    assert!(upgrade.headers.contains(&("Sec-WebSocket-Accept".to_string(), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".to_string())));
    assert_eq!(feed.viewers(), 1);
    assert!(handler.upgrade(&ws_request("wrong"), &stats).is_none());
    let mut plain = ws_request(TOKEN);
    plain.headers.retain(|(k, _)| k != "Sec-WebSocket-Key");
    assert!(handler.upgrade(&plain, &stats).is_none());
    let mut elsewhere = ws_request(TOKEN);
    elsewhere.target = format!("/other?token={TOKEN}");
    assert!(handler.upgrade(&elsewhere, &stats).is_none());
    let second = handler.upgrade(&ws_request(TOKEN), &stats).expect("a second headset");
    assert!(handler.upgrade(&ws_request(TOKEN), &stats).is_none(), "a third has no place");
    assert_eq!(feed.viewers(), MAX_VIEWERS);
    // a headset that leaves, even before it ever connected, gives its place back
    drop(second);
    assert_eq!(feed.viewers(), 1);
    drop(upgrade);
    assert_eq!(feed.viewers(), 0);
}

#[test]
fn the_script_has_no_syntax_errors() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/web/headset.js");
    match Command::new("node").arg("--check").arg(path).output() {
        Ok(out) => assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr)),
        Err(_) => assert!(std::env::var_os("CI").is_none(), "the script check needs Node.js, and CI is set"),
    }
}

#[test]
fn the_script_never_shows_a_picture_that_was_decoding_when_black_came() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/web/headset.js");
    let check = std::env::temp_dir().join(format!("telescope-headset-decode-{}.js", std::process::id()));
    std::fs::write(&check, include_str!("stereo_headset_decode_test.js")).unwrap();
    let ran = Command::new("node").arg(&check).arg(script).output();
    let _ = std::fs::remove_file(&check);
    match ran {
        Ok(out) => assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr)),
        Err(_) => assert!(std::env::var_os("CI").is_none(), "the script check needs Node.js, and CI is set"),
    }
}

#[test]
fn the_script_leaves_vr_on_b_y_or_a_grip_and_not_on_the_trigger() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/web/headset.js");
    let check = std::env::temp_dir().join(format!("telescope-headset-leave-{}.js", std::process::id()));
    std::fs::write(&check, include_str!("stereo_headset_leave_test.js")).unwrap();
    let ran = Command::new("node").arg(&check).arg(script).output();
    let _ = std::fs::remove_file(&check);
    match ran {
        Ok(out) => assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr)),
        Err(_) => assert!(std::env::var_os("CI").is_none(), "the script check needs Node.js, and CI is set"),
    }
}

// ── What a headset is sent ────────────────────────────────────────────────

#[test]
fn a_headset_gets_the_jpeg_made_from_both_eyes_side_by_side() {
    let feed = open_feed();
    let store = Arc::new(Store::new());
    let handler = Headset(feed.clone());
    let (socket, sent, _typed) = pipe(64);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    let worker = {
        let (feed, store) = (feed.clone(), store.clone());
        std::thread::spawn(move || feed.run(&*store, &|_| false))
    };
    // the camera's frame for the pair is out: it is the pair's own
    let pair = job(RED, BLUE, 1);
    let mut made = shot(GREEN);
    made.data = pair.handed.clone();
    made.number = 1;
    *store.outgoing.lock().unwrap() = Some(made);
    assert!(feed.submit(pair));
    let (opcode, payload) = message(&sent);
    assert_eq!(opcode, BINARY);
    assert_eq!(&payload[..2], &[0xFF, 0xD8], "a JPEG");
    let picture = decode(&payload);
    assert_eq!((picture.0, picture.1), (128, 36), "two small eyes side by side, whole");
    assert!(near(px(&picture, 20, 18), RED) && near(px(&picture, 108, 18), BLUE));
    assert_eq!(feed.built(), 1);
    feed.close();
    worker.join().unwrap();
    viewer.join().unwrap();
    assert_eq!(feed.viewers(), 0);
}

#[test]
fn a_headset_that_is_behind_skips_to_the_newest_picture() {
    let feed = open_feed();
    let handler = Headset(feed.clone());
    // every write waits for this side to read it, which is a headset that has stopped taking pictures
    let (socket, sent, typed) = pipe(0);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    let picture = |n: u8| Message::Jpeg(Arc::new(vec![n; 300]));
    feed.publish(picture(1), 0);
    // the viewer took the first picture and waits to send its body
    let head = sent.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(head[0] & 0x0F, BINARY);
    feed.publish(picture(2), 0);
    feed.publish(picture(3), 0);
    assert_eq!(sent.recv_timeout(Duration::from_secs(10)).unwrap()[0], 1);
    typed.send(masked(TEXT, ACK)).unwrap();
    let (opcode, payload) = message(&sent);
    assert_eq!((opcode, payload[0]), (BINARY, 3), "the picture in between was never sent");
    feed.close();
    // the viewer is stopped in a write until it is read, and its end of the pipe goes when it returns
    while sent.recv_timeout(Duration::from_secs(10)).is_ok() {}
    viewer.join().unwrap();
}

#[test]
fn the_next_picture_waits_for_the_headset_to_say_it_decoded_the_last_but_black_does_not() {
    let feed = open_feed();
    let handler = Headset(feed.clone());
    let (socket, sent, typed) = pipe(64);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    let picture = |n: u8| Message::Jpeg(Arc::new(vec![n; 300]));
    feed.publish(picture(1), 0);
    assert_eq!(message(&sent).1[0], 1);
    // no acknowledgement yet: newer pictures are held back, however long they wait
    feed.publish(picture(2), 0);
    assert!(sent.recv_timeout(Duration::from_millis(300)).is_err(), "nothing is sent before the acknowledgement");
    // black goes out at once, it is what hides a picture
    feed.publish(Message::Blank, 0);
    assert_eq!(message(&sent), (TEXT, b"blank".to_vec()));
    feed.publish(picture(3), 0);
    assert!(sent.recv_timeout(Duration::from_millis(100)).is_err());
    // the acknowledgement releases the newest picture, not the one that was skipped
    typed.send(masked(TEXT, ACK)).unwrap();
    assert_eq!(message(&sent), (BINARY, vec![3; 300]));
    assert!(sent.recv_timeout(Duration::from_millis(100)).is_err(), "and the one after waits again");
    feed.close();
    viewer.join().unwrap();
}

#[test]
fn only_the_newest_pair_waits_for_the_worker_and_nothing_is_made_without_a_headset() {
    let feed = open_feed();
    assert!(!feed.wanted() && !feed.submit(job(RED, BLUE, 1)), "no headset, no pair kept");
    assert!(!feed.pending());
    let _slot = feed.claim().expect("a place");
    assert!(feed.wanted() && feed.submit(job(RED, BLUE, 2)) && feed.submit(job(BLUE, RED, 3)));
    assert_eq!(feed.take_job(Duration::ZERO).map(|j| j.number), Some(3));
    assert!(feed.take_job(Duration::ZERO).is_none());
    assert_eq!(feed.built(), 0);
    // and a closed feed takes nothing
    feed.close();
    assert!(!feed.submit(job(RED, BLUE, 4)));
}

#[test]
fn a_viewer_asks_only_for_what_is_newer_than_what_it_had() {
    let feed = open_feed();
    feed.publish(Message::Blank, 0);
    feed.publish(Message::Jpeg(Arc::new(vec![9])), 0);
    let (seq, message) = feed.next(0, Duration::ZERO).expect("the newest");
    assert_eq!(message, Message::Jpeg(Arc::new(vec![9])));
    assert!(feed.next(seq, Duration::from_millis(5)).is_none());
}

/// An open feed that gives up on a missing camera frame after 30 ms instead of 150.
fn quick_feed() -> Feed {
    let mut feed = Feed::new(TOKEN);
    feed.proof_wait = Duration::from_millis(30);
    feed.open();
    feed
}

#[test]
fn a_cover_on_the_camera_covers_the_headset_too() {
    let feed = quick_feed();
    let store = Store::new();
    let pair = job(RED, BLUE, 5);
    // the camera's frame for the pair is out and is what the step made: nothing covers it
    let mut made = shot(GREEN);
    made.data = pair.handed.clone();
    made.number = 5;
    *store.outgoing.lock().unwrap() = Some(made);
    let ours = decode(&feed.picture(&pair, &store, &|_| false).unwrap());
    assert!(near(px(&ours, 20, 18), RED) && near(px(&ours, 108, 18), BLUE));
    // a picture of the same moment that isn't ours (the Wait screen) is what both eyes get
    let mut wait_screen = shot(GREEN);
    wait_screen.number = 5;
    *store.outgoing.lock().unwrap() = Some(wait_screen);
    let covered = decode(&feed.picture(&pair, &store, &|_| false).unwrap());
    assert!(near(px(&covered, 20, 18), GREEN) && near(px(&covered, 108, 18), GREEN));
    // unless it's one of the last this plugin made, which a newer picture has gone out over since
    let kept = decode(&feed.picture(&pair, &store, &|_| true).unwrap());
    assert!(near(px(&kept, 20, 18), RED) && near(px(&kept, 108, 18), BLUE));
}

#[test]
fn a_pair_is_never_sent_without_the_camera_frame_that_shows_whether_it_was_covered() {
    let feed = quick_feed();
    let store = Store::new();
    let pair = job(RED, BLUE, 5);
    // no frame out at all: the first frame of a stream
    assert!(feed.picture(&pair, &store, &|_| false).is_none(), "no frame out, nothing is sent");
    // the frame before the pair is out, and a cover is on it: the pair's own frame isn't out to say it has none
    let mut before = shot(GREEN);
    before.number = 4;
    *store.outgoing.lock().unwrap() = Some(before);
    assert!(feed.picture(&pair, &store, &|_| false).is_none(), "an older frame proves nothing");
    // a covered frame that can't be read is no proof either
    let mut unreadable = Frame::new(64, 36, PixelFormat::I420, vec![0; 64 * 36 * 3 / 2]);
    unreadable.data = Arc::new(Vec::new());
    unreadable.number = 5;
    *store.outgoing.lock().unwrap() = Some(unreadable);
    assert!(feed.picture(&pair, &store, &|_| false).is_none(), "a frame that can't be read isn't sent as the pair");
    assert_eq!(feed.built(), 0);
}

#[test]
fn a_pair_waits_for_its_frame_to_come_out_and_gets_the_cover_that_came_on_it() {
    let feed = Arc::new(Feed::new(TOKEN));
    feed.open();
    let store = Arc::new(Store::new());
    let pair = job(RED, BLUE, 5);
    // the pipeline finishes the frame a little after the pair was handed over, and the Wait screen came on it
    let late = {
        let store = store.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            let mut wait_screen = shot(GREEN);
            wait_screen.number = 5;
            *store.outgoing.lock().unwrap() = Some(wait_screen);
        })
    };
    let picture = decode(&feed.picture(&pair, &*store, &|_| false).expect("a picture once the frame is out"));
    assert!(near(px(&picture, 20, 18), GREEN) && near(px(&picture, 108, 18), GREEN), "the pair never goes out clear");
    late.join().unwrap();
}

#[test]
fn the_worker_publishes_nothing_while_the_camera_frame_is_missing() {
    let feed = Arc::new(quick_feed());
    feed.open();
    let store = Arc::new(Store::new());
    let _slot = feed.claim().expect("a place");
    let worker = {
        let (feed, store) = (feed.clone(), store.clone());
        std::thread::spawn(move || feed.run(&*store, &|_| false))
    };
    assert!(feed.submit(job(RED, BLUE, 1)));
    assert!(feed.next(0, Duration::from_millis(300)).is_none(), "nothing sent without the frame");
    assert_eq!(feed.built(), 0);
    // a later pair whose frame is out is sent
    let next = job(RED, BLUE, 2);
    let mut made = shot(GREEN);
    made.data = next.handed.clone();
    made.number = 2;
    *store.outgoing.lock().unwrap() = Some(made);
    assert!(feed.submit(next));
    assert!(matches!(feed.next(0, Duration::from_secs(10)), Some((_, Message::Jpeg(_)))));
    feed.close();
    worker.join().unwrap();
}

#[test]
fn a_picture_that_is_old_or_from_before_a_hiding_step_gives_way_to_black() {
    let feed = open_feed();
    feed.publish(Message::Jpeg(Arc::new(vec![1])), 3);
    feed.settle(3);
    let (seq, first) = feed.next(0, Duration::ZERO).unwrap();
    assert!(matches!(first, Message::Jpeg(_)), "as new as the epoch and as the clock");
    // a hiding step came on since
    feed.settle(4);
    let (seq, second) = feed.next(seq, Duration::ZERO).unwrap();
    assert_eq!(second, Message::Blank);
    // black stays black
    feed.settle(5);
    assert!(feed.next(seq, Duration::ZERO).is_none());
    feed.publish(Message::Jpeg(Arc::new(vec![1])), 5);
    feed.lock().shown.as_mut().unwrap().at = Instant::now() - STALE - Duration::from_secs(1);
    feed.settle(5);
    assert_eq!(feed.next(seq + 1, Duration::ZERO).unwrap().1, Message::Blank);
}

#[test]
fn a_new_link_disconnects_the_headsets_on_the_old_one() {
    let feed = open_feed();
    let handler = Headset(feed.clone());
    let (socket, sent, _typed) = pipe(64);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    feed.set_token("zyxwvutsrqponmlkjihgfedc");
    let (opcode, payload) = message(&sent);
    assert_eq!((opcode, payload), (CLOSE, 1001u16.to_be_bytes().to_vec()));
    viewer.join().unwrap();
    assert!(!feed.token_ok(TOKEN) && feed.token_ok("zyxwvutsrqponmlkjihgfedc"));
    assert!(handler.upgrade(&ws_request(TOKEN), &stats()).is_none(), "the old link gets nothing");
}

#[test]
fn the_link_and_its_generation_are_taken_together_while_links_change() {
    let feed = Arc::new(Feed::new("token-0000000000000000000"));
    feed.open();
    let rotating = {
        let feed = feed.clone();
        std::thread::spawn(move || {
            for n in 1..=2000u64 {
                feed.set_token(&format!("token-{n:019}"));
            }
        })
    };
    let mut admitted = 0;
    while !rotating.is_finished() {
        let token = feed.lock().token.clone();
        if let Some(slot) = feed.admit(&token) {
            admitted += 1;
            // link number n is the nth one made, so the generation a headset holds is its own link's number whatever rotated meanwhile
            let n: u64 = token[6..].parse().unwrap();
            assert_eq!(slot.generation, n);
        }
    }
    rotating.join().unwrap();
    assert!(admitted > 0);
    // exactly: the token passed in is the one the generation belongs to
    feed.set_token("token-fixed-link-123456");
    let slot = feed.admit("token-fixed-link-123456").expect("a place");
    assert!(feed.current(&slot));
    assert!(feed.admit("token-0000000000000000000").is_none(), "an old link gets nothing");
    feed.set_token("token-newer-link-1234567");
    assert!(!feed.current(&slot), "a headset on the old link is over");
}

#[test]
fn a_headset_stuck_in_a_write_does_not_survive_the_feed_being_closed_and_opened_again() {
    let feed = open_feed();
    let handler = Headset(feed.clone());
    // every write waits for this side to read it: a headset that has stopped taking pictures
    let (socket, sent, _typed) = pipe(0);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    feed.publish(Message::Jpeg(Arc::new(vec![1; 300])), 0);
    // it has the picture and is stopped in its write
    assert_eq!(sent.recv_timeout(Duration::from_secs(10)).unwrap()[0] & 0x0F, BINARY);
    assert_eq!(feed.viewers(), 1);
    feed.close();
    feed.open();
    // the new run has its places, and the old headset is no part of it
    assert_eq!((feed.viewers(), feed.wanted()), (0, false));
    let (first, second) = (feed.claim().expect("a place"), feed.claim().expect("a second place"));
    assert!(feed.claim().is_none() && feed.viewers() == MAX_VIEWERS);
    // it can't be sent what the new run publishes, and it leaves once its write goes through
    feed.publish(Message::Jpeg(Arc::new(vec![2; 300])), 0);
    while sent.recv_timeout(Duration::from_secs(10)).is_ok() {}
    viewer.join().unwrap();
    assert_eq!(feed.viewers(), MAX_VIEWERS, "the old headset giving up its place doesn't take one of the new run's");
    drop((first, second));
    assert_eq!(feed.viewers(), 0);
}

#[test]
fn a_ping_is_answered_and_a_close_ends_the_viewer_and_gives_its_place_back() {
    let feed = open_feed();
    let handler = Headset(feed.clone());
    let (socket, sent, typed) = pipe(64);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    typed.send(masked(PING, b"hi")).unwrap();
    assert_eq!(message(&sent), (PONG, b"hi".to_vec()));
    typed.send(masked(CLOSE, &[])).unwrap();
    viewer.join().unwrap();
    assert_eq!(feed.viewers(), 0);
}

#[test]
fn the_packed_picture_is_two_eyes_wide_up_to_the_cap() {
    assert_eq!(packed_size(1920, 1080, MAX_PACKED), (1920, 1080), "two 1080p eyes get half their width each");
    assert_eq!(packed_size(640, 360, MAX_PACKED), (1280, 360), "small eyes are sent whole");
    assert_eq!(packed_size(3840, 2160, MAX_PACKED), (1920, 1080));
    assert_eq!(packed_size(1081, 607, MAX_PACKED), (1920, 606), "both sides even");
    assert_eq!(packed_size(0, 0, MAX_PACKED), (2, 2));
    assert_eq!(packed_size(1920, 1080, (3840, 1080)), (3840, 1080), "the cap is one number to turn up");
}

/// How long one 3840x1080 and one 1920x1080 picture take to pack and encode, for `cargo test --release -p telescope-effects headset_speed -- --ignored --nocapture`.
#[test]
#[ignore]
fn headset_speed() {
    let eye = |seed: u8| {
        // a soft pattern with a little noise: harder than a flat wall and kinder than static
        let data: Vec<u8> = (0..1920 * 1080 * 4)
            .map(|i| {
                let (c, x, y) = ((i % 4) as f32, ((i / 4) % 1920) as f32, ((i / 4) / 1920) as f32);
                let noise = ((i as u32).wrapping_mul(2654435761) >> 28) as f32 - 8.0;
                let v = 128.0 + 70.0 * ((x + seed as f32 * 11.0) / 37.0 + c).sin() * (y / 53.0).cos() + noise;
                if c == 3.0 { 255 } else { v.clamp(0.0, 255.0) as u8 }
            })
            .collect();
        Frame::new(1920, 1080, PixelFormat::Rgba, data)
    };
    let (l, r) = (eye(1), eye(2));
    let (left, right) = (Rgb::from_frame(&l).unwrap(), Rgb::from_frame(&r).unwrap());
    let tune = Tune { depth: 1.0, height: 0.5, size: 92.3, tilt_left: -0.8, tilt_right: 1.2, lut: None };
    for (name, tune) in [("plain", Tune::default()), ("fine tune", tune)] {
        for (label, size) in [("3840x1080", (3840, 1080)), ("1920x1080", (1920, 1080))] {
            let mut pack = Vec::new();
            let mut encoded = Vec::new();
            let mut bytes = 0;
            for round in 0..25 {
                let t = Instant::now();
                let packed = stereo::side_by_side(&left, &right, size.0, size.1, &tune);
                let packing = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let jpeg = encode(&packed, QUALITY).unwrap();
                let encoding = t.elapsed().as_secs_f64() * 1000.0;
                if round >= 5 {
                    pack.push(packing);
                    encoded.push(encoding);
                    bytes = jpeg.len();
                }
            }
            pack.sort_by(|a, b| a.total_cmp(b));
            encoded.sort_by(|a, b| a.total_cmp(b));
            println!("{name:<10} {label}: pack p50 {:.1} ms, encode p50 {:.1} ms (p90 {:.1}), {} KB", pack[10], encoded[10], encoded[18], bytes / 1024);
        }
    }
}

#[test]
fn a_picture_made_before_a_hiding_step_came_on_is_sent_as_black() {
    let feed = open_feed();
    let store = Arc::new(Store::new());
    store.epoch.store(1, Ordering::SeqCst);
    feed.watch_epochs(store);
    // the worker hasn't woken to blank it yet
    feed.publish(Message::Jpeg(Arc::new(vec![0xFF, 0xD8, 1, 2, 3])), 0);
    let handler = Headset(feed.clone());
    let (socket, sent, _typed) = pipe(64);
    let upgrade = handler.upgrade(&ws_request(TOKEN), &stats()).expect("an upgrade");
    let viewer = std::thread::spawn(move || (upgrade.run)(socket));
    let (opcode, payload) = message(&sent);
    assert_eq!((opcode, payload.as_slice()), (TEXT, b"blank".as_slice()));
    feed.close();
    viewer.join().unwrap();
}
