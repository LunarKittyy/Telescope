use super::*;
use crate::pixels::gaussian_blur;
use std::sync::Mutex;
use telescope_core::testing::FakeHost;

fn wifi(ssid: &str, password: &str, security: &str, hidden: bool) -> Option<Wifi> {
    Some(Wifi { ssid: ssid.into(), password: password.into(), security: security.into(), hidden })
}

#[test]
fn wifi_codes_are_read_with_their_escapes() {
    assert_eq!(parse_wifi("WIFI:T:WPA;S:Home;P:hunter2;;"), wifi("Home", "hunter2", "WPA", false));
    assert_eq!(parse_wifi(r"WIFI:S:My\;Net\:5G;T:WPA;P:pa\\ss\,word;H:true;;"), wifi("My;Net:5G", "pa\\ss,word", "WPA", true));
    assert_eq!(parse_wifi("wifi:T:nopass;S:Cafe;;"), wifi("Cafe", "", "nopass", false));
    assert_eq!(parse_wifi("WIFI:T:WPA;P:nameless;;"), None);
    assert_eq!(parse_wifi("https://example.com"), None);
    // The first of a key counts, keys are any case and the last field needs no ; after it
    assert_eq!(parse_wifi("WIFI:S:a;S:b;P:1;P:2;;"), wifi("a", "1", "", false));
    assert_eq!(parse_wifi("WIFI:s:lower;p:pw;t:WEP;h:TRUE"), wifi("lower", "pw", "WEP", true));
    assert_eq!(parse_wifi("WIFI:S:\u{e9}\u{e8};P:\u{4e2d}\u{6587};;"), wifi("\u{e9}\u{e8}", "\u{4e2d}\u{6587}", "", false));
    assert_eq!(parse_wifi("WIFI:S:;P:x;;"), None);
}

#[test]
fn what_goes_to_the_clipboard() {
    let (password, line, _note) = QrReaderPlugin::copy("WIFI:T:WPA;S:Home;P:hunter2;;");
    assert!(password == "hunter2" && line.contains("Home") && !line.contains("hunter2"), "the card never shows the password");
    let (name, line, _note) = QrReaderPlugin::copy("WIFI:T:nopass;S:Cafe;;");
    assert!(name == "Cafe" && line.contains("no password"));
    let (link, line, note) = QrReaderPlugin::copy("https://example.com/a");
    assert!(link == note && link == "https://example.com/a" && line.contains("example.com"));
    assert_eq!(QrReaderPlugin::copy("just some text").0, "just some text");
    assert!(is_link("HTTPS://x.y") && !is_link("ftp://x.y"));
}

// ── Looking at a picture ─────────────────────────────────────────────────────

/// A grey picture with the code for `text` at (x, y), each module `scale` pixels with a quiet zone of four.
fn code_picture(text: &str, scale: usize, (w, h): (usize, usize), (x, y): (usize, usize), shade: u8) -> Vec<u8> {
    let (cells, size) = telescope_core::qr::encode(text);
    let side = (size + 8) * scale;
    let mut grey = vec![shade; w * h];
    for row in 0..side {
        for col in 0..side {
            let (r, c) = ((row / scale) as i64 - 4, (col / scale) as i64 - 4);
            let dark = r >= 0 && c >= 0 && (r as usize) < size && (c as usize) < size && cells.contains(&(r as usize * size + c as usize));
            grey[(y + row) * w + x + col] = if dark { 0 } else { 255 };
        }
    }
    grey
}

#[test]
fn codes_are_read_from_arms_length_to_right_up_close() {
    for scale in [4, 8, 14] {
        let grey = gaussian_blur(&code_picture("https://example.com", scale, (1280, 720), (400, 10), 90), 1280, 720, 1.0);
        let (text, corners) = read_code(&grey, 1280, 720).unwrap_or_else(|| panic!("no code at {scale}x"));
        assert_eq!(text, "https://example.com", "{scale}x");
        assert!(0.25 < corners.iter().map(|c| c[0]).fold(f64::INFINITY, f64::min) && corners.iter().map(|c| c[0]).fold(f64::INFINITY, f64::min) < 0.45, "{scale}x");
    }
}

// Codes recorded from Python: the modules of qrcode's version, and what cv2's detectAndDecode gave for them.
type Recorded = (usize, &'static [u64]);
// Python: 'WIFI:S:café;P:sécret;;' from Latin-1 bytes with no ECI
const LATIN1_WIFI: Recorded = (25, &[0x1fd987f, 0x104f941, 0x1755d5d, 0x1744c5d, 0x1749c5d, 0x105b541, 0x1fd557f, 0xa400, 0x1469125, 0x1524768, 0x1d42859, 0x13924a, 0x1c5262e, 0xda8f64, 0x195e785, 0x6a4683, 0x1f5f5f1, 0x11f16, 0x1fd1559, 0x1043519, 0x174cff5, 0x174c812, 0x175a313, 0x1048a50, 0x1fdb921]);
// Python: 'WIFI:S:café;P:sécret;;' from UTF-8 bytes
const UTF8_WIFI: Recorded = (25, &[0x1fd157f, 0x105df41, 0x1754d5d, 0x174895d, 0x175f75d, 0x1042a41, 0x1fd557f, 0x8a00, 0x13f9697, 0xa2023d, 0x1fdce7d, 0xb8838d, 0x12e202e, 0x182de55, 0x1f44f57, 0x151d196, 0x1643bf5, 0x12711, 0x1fd5b59, 0x105af19, 0x17533f4, 0x1759a07, 0x174efb7, 0x1041697, 0x1fd0761]);
// Python: 'abcÃ' from the bytes 61 62 63 C3
const TRUNCATED_UTF8: Recorded = (21, &[0x1fc57f, 0x104b41, 0x17595d, 0x175d5d, 0x17575d, 0x105d41, 0x1fd57f, 0x1300, 0x17c17c, 0x59125, 0x1f6292, 0x1db43e, 0xe7a90, 0x17c5, 0x1fc96a, 0x105bc8, 0x175526, 0x175124, 0x175a98, 0x104434, 0x1fd296]);
// Python: UnicodeDecodeError, for a Kanji segment of 日本語 (OpenCV leaves it as Shift-JIS)
const KANJI: Recorded = (21, &[0x1fc07f, 0x104441, 0x17555d, 0x17565d, 0x17515d, 0x105641, 0x1fd57f, 0x1c00, 0x17c27c, 0xdb3c6, 0x1ed56b, 0xd03ca, 0x16e528, 0x112b, 0x1fc692, 0x10543b, 0x175a9c, 0x175be4, 0x17556c, 0x104fe8, 0x1fd90d]);
// Python: UnicodeDecodeError, for the bytes C0 80 (OpenCV takes them for UTF-8, Python doesn't)
const OVERLONG: Recorded = (21, &[0x1fcb7f, 0x104541, 0x17535d, 0x17565d, 0x175b5d, 0x105941, 0x1fd57f, 0x1f00, 0x17c57c, 0x199524, 0x17f695, 0x33434, 0x1efa94, 0x13c8, 0x1fc163, 0x1057c9, 0x175924, 0x175524, 0x175e9c, 0x104834, 0x1fd29e]);

fn recorded_picture((size, rows): Recorded) -> Vec<u8> {
    let (scale, side) = (6, size + 8);
    let (w, h) = (side * scale, side * scale);
    let mut grey = vec![255u8; w * h];
    for (r, row) in rows.iter().enumerate() {
        for c in 0..size {
            if row >> (size - 1 - c) & 1 == 1 {
                for (y, x) in (0..scale).flat_map(|y| (0..scale).map(move |x| (y, x))) {
                    grey[((r + 4) * scale + y) * w + (c + 4) * scale + x] = 0;
                }
            }
        }
    }
    grey
}

fn read_recorded(code: Recorded) -> Result<Option<String>, Unreadable> {
    let side = (code.0 + 8) * 6;
    try_read_code(&recorded_picture(code), side, side).map(|found| found.map(|(text, _)| text))
}

#[test]
fn a_latin_1_code_reads_as_latin_1_where_utf_8_fails() {
    assert_eq!(read_recorded(LATIN1_WIFI), Ok(Some("WIFI:S:café;P:sécret;;".into())));
    assert_eq!(read_recorded(UTF8_WIFI), Ok(Some("WIFI:S:café;P:sécret;;".into())));
    // Bytes that stop short of what their lead byte says are Latin-1 too
    assert_eq!(read_recorded(TRUNCATED_UTF8), Ok(Some("abcÃ".into())));
    assert_eq!(read_code(&recorded_picture(LATIN1_WIFI), 198, 198).unwrap().0, "WIFI:S:café;P:sécret;;");
}

#[test]
fn a_code_python_cant_turn_into_text_reports_nothing() {
    assert_eq!(read_recorded(KANJI), Err(Unreadable));
    assert_eq!(read_recorded(OVERLONG), Err(Unreadable));
}

#[test]
fn no_code_is_none() {
    assert!(read_code(&vec![90; 160 * 90], 160, 90).is_none());
    assert!(read_code(&[], 0, 0).is_none());
}

#[test]
fn a_picture_is_shrunk_by_what_each_new_pixel_covers() {
    assert_eq!(area_resize(&[0, 100, 200, 100], 2, 2, 1, 1), vec![100]);
    assert_eq!(area_resize(&[10, 20, 30], 3, 1, 3, 1), vec![10, 20, 30]);
    // A third of a pixel each side: 3 pixels into 2
    assert_eq!(area_resize(&[0, 90, 0], 3, 1, 2, 1), vec![30, 30]);
    assert_eq!(area_resize(&[255; 25], 5, 5, 3, 3), vec![255; 9]);
}

#[test]
fn a_picture_goes_grey_the_way_the_viewer_sees_it() {
    let mut y: Vec<u8> = vec![16, 126, 235, 16];
    y.extend([128; 2]);
    let video = Frame::new(2, 2, PixelFormat::I420, y);
    assert_eq!(grey_of(&video), vec![0, 128, 255, 0]);
    // cv2.cvtColor(.., COLOR_BGR2GRAY) of the colour: (255, 0, 0) and (0, 255, 0) as RGB
    let rgba = Frame::new(1, 2, PixelFormat::Rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    assert_eq!(grey_of(&rgba), vec![76, 150]);
    let bgra = Frame::new(1, 1, PixelFormat::Bgra, vec![0, 0, 255, 255]);
    assert_eq!(grey_of(&bgra), vec![76]);
    // Python: Y 81, U 16, V 16 is BGR (0, 211, 0) once the red and blue are clipped, and grey 124, where luma alone says 76
    let green = Frame::new(2, 2, PixelFormat::I420, vec![81, 81, 81, 81, 16, 16]);
    assert_eq!(grey_of(&green), vec![124; 4]);
    let nv12 = Frame::new(2, 2, PixelFormat::Nv12, vec![81, 81, 81, 81, 16, 16]);
    assert_eq!(grey_of(&nv12), vec![124; 4]);
}

// ── Quads that look like codes, and ones that stay ───────────────────────────

fn quad(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    points.to_vec()
}

#[test]
fn a_quad_has_to_look_like_a_code() {
    assert!(plausible(&quad(&[[100., 100.], [200., 100.], [200., 200.], [100., 200.]])));
    assert!(plausible(&quad(&[[100., 100.], [200., 110.], [190., 190.], [110., 200.]])), "held at an angle");
    assert!(!plausible(&quad(&[[100., 100.], [200., 100.], [120., 120.], [100., 200.]])), "caved in");
    assert!(!plausible(&quad(&[[100., 100.], [200., 200.], [200., 100.], [100., 200.]])), "crossed over");
    assert!(!plausible(&quad(&[[100., 100.], [400., 100.], [400., 112.], [100., 112.]])), "a sliver");
    assert!(!plausible(&quad(&[[100., 100.], [108., 100.], [108., 108.], [100., 108.]])), "a speck");
    assert!(!plausible(&quad(&[[100., 100.], [200., 100.], [200., 200.]])), "three corners");
    assert!(!plausible(&quad(&[[f64::NAN, 0.], [100., 0.], [100., 100.], [0., 100.]])));
    assert!(!plausible(&quad(&[[0., 0.], [1e9, 0.], [f64::INFINITY, 1e9], [0., 1e9]])));
}

const QUAD: Corners = [[0.4, 0.2], [0.6, 0.2], [0.6, 0.55], [0.4, 0.55]];

fn moved(by: [f64; 2]) -> Corners {
    QUAD.map(|p| [p[0] + by[0], p[1] + by[1]])
}

#[test]
fn one_look_at_a_stray_quad_never_counts() {
    let mut steady = Steady::new(3, 1);
    assert!(steady.see("", Some(QUAD)).is_none(), "seen but not read");
    assert!(steady.see("noise", Some(QUAD)).is_none());
    assert!(steady.see("", None).is_none());
    assert!(steady.see("noise", Some(QUAD)).is_none() && !steady.confirmed(), "a miss in between starts it over");
    assert!(steady.see("other", Some(QUAD)).is_none() && steady.see("noise", Some(moved([0.5, 0.5]))).is_none(), "changed or jumped");
}

#[test]
fn a_steady_code_counts_after_a_few_looks_and_rides_out_a_miss() {
    let mut steady = Steady::new(3, 1);
    assert!(steady.see("hi", Some(QUAD)).is_none() && steady.pending());
    assert!(steady.see("hi", Some(moved([0.01, 0.01]))).is_none());
    let (text, corners) = steady.see("hi", Some(moved([0.02, 0.02]))).unwrap();
    assert!(text == "hi" && corners == moved([0.02, 0.02]) && !steady.pending());
    assert_eq!(steady.see("", None), Some(("hi".to_string(), corners)), "one missed look keeps it");
    assert_eq!(steady.see("hi", Some(QUAD)).unwrap().0, "hi");
    assert!(steady.see("", None).is_some() && steady.see("", None).is_none(), "two in a row lets it go");
}

#[test]
fn a_moving_code_stays_outlined() {
    let mut steady = Steady::new(3, 1);
    let mut found = None;
    for i in 0..12 {
        found = steady.see("hi", Some(moved([0.04 * i as f64, 0.0])));
        assert_eq!(found.is_none(), i < 2, "{i}");
    }
    assert_eq!(found.unwrap().1, moved([0.44, 0.0]));
    assert_eq!(steady.see("hi", Some(moved([-0.3, -0.3]))).unwrap().0, "hi", "a fast jump of the same code once it's confirmed");
}

// ── The plugin ───────────────────────────────────────────────────────────────

struct Rig {
    plugin: QrReaderPlugin,
    host: FakeHost,
    now: Arc<Mutex<f64>>,
}

impl Rig {
    fn new() -> Rig {
        let now = Arc::new(Mutex::new(100.0));
        let clock = now.clone();
        let mut rig = Rig { plugin: QrReaderPlugin::with_clock(move || *clock.lock().unwrap()), host: FakeHost::new(), now };
        rig.plugin.setup(&mut rig.host);
        rig.event(Event::PhonesChanged { count: 1 });
        rig.set_config(json!({}));
        rig
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
    }

    fn set_config(&mut self, cfg: Value) {
        self.plugin.set_config(&cfg).unwrap();
        // The next hook with a host
        self.event(Event::StreamStopped);
    }

    fn found(&mut self, text: &str, corners: Option<Corners>) {
        self.event(Event::Posted { owner: NAME.into(), token: "found".into(), data: json!({"text": text, "corners": corners}) });
    }

    fn timer(&mut self, token: &str) {
        self.event(Event::TimerFired { owner: NAME.into(), token: token.into() });
    }

    fn read(&mut self, text: &str) {
        for _ in 0..3 {
            self.found(text, Some(QUAD));
        }
    }

    fn calls(&self, start: &str) -> Vec<String> {
        self.host.calls.iter().filter(|c| c.starts_with(start)).cloned().collect()
    }

    /// The card's properties as the last things said about them leave them.
    fn props(&self) -> Value {
        let mut all = serde_json::Map::new();
        for call in self.calls("set_ui_props card ") {
            let Value::Object(props) = serde_json::from_str::<Value>(&call["set_ui_props card ".len()..]).unwrap() else { panic!() };
            all.extend(props);
        }
        Value::Object(all)
    }

    fn outline(&self) -> usize {
        self.host.layers.get(NAME).map_or(0, |l| l.shapes.len())
    }

    fn notified(&self) -> Vec<String> {
        self.calls("send_notification")
    }
}

struct NoContext;

impl FrameContext for NoContext {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A video frame with the code in it: luma in the video's range, no colour.
fn video_frame(text: &str) -> Frame {
    let (w, h) = (640, 360);
    let grey = code_picture(text, 6, (w, h), (300, 40), 90);
    let mut data: Vec<u8> = grey.iter().map(|g| (*g as f32 * 219.0 / 255.0 + 16.0).round() as u8).collect();
    data.extend(vec![128; 2 * (w / 2) * (h / 2)]);
    Frame::new(w as u32, h as u32, PixelFormat::I420, data)
}

fn cpu(step: Option<FrameStep>) -> Box<dyn CpuStep> {
    match step {
        Some(FrameStep::Cpu(step)) => step,
        other => panic!("a Rust step: {other:?}"),
    }
}

/// Runs the frame through the step and waits for its thread's answer (what it posted).
fn look(step: &mut dyn CpuStep, frame: &Frame, host: &FakeHost) -> Value {
    let mut copy = frame.clone();
    step.process(&mut copy, &NoContext);
    assert!(std::sync::Arc::ptr_eq(&copy.data, &frame.data), "the frame goes on unchanged");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some((token, data)) = host.take_posted().into_iter().next() {
            assert_eq!(token, "found");
            return data;
        }
        assert!(Instant::now() < deadline, "no answer from the look");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_code_in_the_picture_goes_to_the_clipboard_once() {
    let mut rig = Rig::new();
    let frame = video_frame("WIFI:T:WPA;S:Telescope;P:hunter22;;");
    let mut step = cpu(rig.plugin.frame_step());
    // Off: nothing looked at
    step.process(&mut frame.clone(), &NoContext);
    std::thread::sleep(Duration::from_millis(50));
    assert!(rig.host.take_posted().is_empty());
    rig.set_config(json!({"on": true}));
    assert!(rig.host.layers.contains_key(NAME));
    // One look isn't enough to outline it
    let answer = look(&mut *step, &frame, &rig.host);
    assert_eq!(answer["text"], "WIFI:T:WPA;S:Telescope;P:hunter22;;");
    rig.event(Event::Posted { owner: NAME.into(), token: "found".into(), data: answer });
    assert!(rig.outline() == 0 && rig.plugin.steady.pending());
    for _ in 0..2 {
        // While a code waits to be confirmed it's looked at sooner
        std::thread::sleep(Duration::from_millis(120));
        let answer = look(&mut *step, &frame, &rig.host);
        rig.event(Event::Posted { owner: NAME.into(), token: "found".into(), data: answer });
    }
    assert_eq!(rig.host.clipboard.as_deref(), Some("hunter22"));
    assert!(rig.host.is_scheduled("check") && rig.outline() == 2);
    assert!(rig.props()["copy-shown"] == true);
    rig.timer("check");
    assert_eq!(rig.notified(), ["send_notification QR code copied"]);
    assert_eq!(rig.props()["status"], "Copied the password for Telescope");
    assert_eq!(rig.props()["status-kind"], "status_ok");
    // Still in view: not copied over and over
    rig.host.clipboard = Some("something else".into());
    *rig.now.lock().unwrap() += 1.0;
    rig.found("WIFI:T:WPA;S:Telescope;P:hunter22;;", Some(QUAD));
    assert_eq!(rig.host.clipboard.as_deref(), Some("something else"));
    // Out of view for a while and back: copied again
    *rig.now.lock().unwrap() += 10.0;
    rig.found("WIFI:T:WPA;S:Telescope;P:hunter22;;", Some(QUAD));
    assert_eq!(rig.host.clipboard.as_deref(), Some("hunter22"));
}

#[test]
fn a_wide_frame_is_shrunk_before_it_is_looked_at() {
    let mut rig = Rig::new();
    rig.set_config(json!({"on": true}));
    let mut step = cpu(rig.plugin.frame_step());
    let big = Frame::new(2560, 1440, PixelFormat::Rgba, vec![90; 2560 * 1440 * 4]);
    let answer = look(&mut *step, &big, &rig.host);
    assert_eq!((answer["text"].as_str(), answer["corners"].is_null()), (Some(""), true));
}

#[test]
fn a_stray_quad_is_never_outlined() {
    let mut rig = Rig::new();
    rig.set_config(json!({"on": true}));
    for _ in 0..5 {
        rig.found("", Some(QUAD));
        rig.found("", None);
    }
    assert!(rig.outline() == 0 && rig.notified().is_empty());
}

#[test]
fn notifications_can_be_switched_off() {
    let mut rig = Rig::new();
    rig.set_config(json!({"on": true, "notify": false}));
    rig.read("https://example.com");
    rig.timer("check");
    assert!(rig.notified().is_empty());
    assert!(rig.props()["status"].as_str().unwrap().contains("example.com"));
    assert_eq!(rig.plugin.get_config(), json!({"on": true, "notify": false}));
}

#[test]
fn switching_off_takes_the_outline_off_the_preview() {
    let mut rig = Rig::new();
    rig.set_config(json!({"on": true}));
    rig.plugin.on_ui(&mut rig.host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(false)] });
    assert!(!rig.host.layers.contains_key(NAME));
    rig.read("https://example.com");
    assert!(rig.notified().is_empty() && rig.host.clipboard.is_none(), "a result that comes in after switching off is dropped");
    assert_eq!(rig.plugin.get_config()["on"], false);
}

#[test]
fn the_outline_goes_when_no_look_confirms_it() {
    let mut rig = Rig::new();
    rig.set_config(json!({"on": true}));
    rig.read("https://example.com");
    assert_eq!(rig.outline(), 2);
    assert!(rig.host.scheduled.contains(&(1000, "outline".to_string())));
    rig.timer("outline");
    assert_eq!(rig.outline(), 0, "the frames stopped coming");
    // A miss in view takes it off at once
    rig.read("https://example.com");
    rig.found("", None);
    assert_eq!(rig.outline(), 2, "one miss is ridden out");
    rig.found("", None);
    assert_eq!(rig.outline(), 0);
    assert!(!rig.host.is_scheduled("outline"));
}

#[test]
fn the_outline_is_dark_under_green() {
    let mut rig = Rig::new();
    rig.set_config(json!({"on": true}));
    rig.read("https://example.com");
    let shapes = &rig.host.layers[NAME].shapes;
    let Shape::Polygon { points, stroke, width, fill } = &shapes[0] else { panic!() };
    assert_eq!((stroke.as_str(), *width, fill.is_none(), points.len()), ("#0000008c", 5.0, true, 4));
    let Shape::Polygon { stroke, width, .. } = &shapes[1] else { panic!() };
    assert_eq!((stroke.as_str(), *width), ("#5fc98c", 3.0));
}

#[test]
fn a_copy_wayland_ignores_says_to_click_copy() {
    let mut rig = Rig::new();
    rig.host.clipboard = Some("old".into());
    rig.host.clipboard_deaf = true;
    rig.set_config(json!({"on": true}));
    rig.read("https://example.com");
    rig.timer("check");
    assert_eq!(rig.host.clipboard.as_deref(), Some("old"));
    assert!(rig.props()["status"].as_str().unwrap().contains("Click Copy"));
    assert_eq!(rig.props()["status-kind"], "status_warn");
    assert_eq!(rig.notified(), ["send_notification QR code found"]);
    rig.host.clipboard_deaf = false;
    rig.plugin.on_ui(&mut rig.host, &UiEvent { ui: "card".into(), callback: "copy-clicked".into(), args: vec![] });
    assert_eq!(rig.host.clipboard.as_deref(), Some("https://example.com"));
    assert!(rig.props()["status"].as_str().unwrap().contains("example.com"));
}

#[test]
fn a_host_that_cant_read_the_clipboard_is_taken_at_its_word() {
    let mut rig = Rig::new();
    rig.host.clipboard_deaf = true;
    rig.set_config(json!({"on": true}));
    rig.read("https://example.com");
    rig.timer("check");
    // The fake host answers None for an empty clipboard, as one that can't say does
    assert_eq!(rig.notified(), ["send_notification QR code copied"]);
}

#[test]
fn loading_settings_switches_on_once() {
    let mut rig = Rig::new();
    let before = rig.calls("schedule_save").len();
    rig.set_config(json!({"on": true}));
    assert_eq!(rig.calls("schedule_save").len() - before, 1);
    assert!(rig.host.layers.contains_key(NAME));
    assert_eq!(rig.props()["on"], true);
}

#[test]
fn the_card_starts_with_the_hint_and_wakes_with_a_phone() {
    let mut rig = Rig::new();
    assert_eq!(rig.props()["active"], true);
    rig.event(Event::PhonesChanged { count: 0 });
    assert_eq!(rig.props()["active"], false);
    rig.set_config(json!({"on": true}));
    assert_eq!(rig.props()["status"], "Hold a QR code up to the camera.");
    rig.set_config(json!({"on": false}));
    assert_eq!(rig.props()["status"], "");
}

#[test]
fn the_shortcut_switches_it_on_and_off() {
    let mut rig = Rig::new();
    let action = rig.plugin.actions().remove(0);
    assert_eq!((action.id.as_str(), action.label.as_str(), action.group.as_str()), ("qr_reader.on", "Read QR codes", "QR code reader"));
    let said = rig.plugin.run_action(&mut rig.host, "qr_reader.on", &json!({}));
    assert_eq!(said.as_deref(), Some("Read QR codes: on"));
    assert_eq!(rig.plugin.get_config()["on"], true);
    assert!(rig.host.layers.contains_key(NAME));
    rig.plugin.run_action(&mut rig.host, "qr_reader.on", &json!({"mode": "off"}));
    assert_eq!(rig.plugin.get_config()["on"], false);
    // Held down: on while held, back to where it was after
    rig.plugin.run_action(&mut rig.host, "qr_reader.on", &json!({"mode": "hold"}));
    assert_eq!(rig.plugin.get_config()["on"], true);
    rig.plugin.release_action(&mut rig.host, "qr_reader.on", &json!({"mode": "hold"}));
    assert_eq!(rig.plugin.get_config()["on"], false);
    assert!(rig.plugin.run_action(&mut rig.host, "qr_reader.other", &json!({})).is_none());
}

#[test]
fn what_it_says_in_diagnostics() {
    let mut rig = Rig::new();
    assert_eq!(rig.plugin.diagnostics(), [("QR code reader".to_string(), "off".to_string())]);
    rig.set_config(json!({"on": true}));
    assert_eq!(rig.plugin.diagnostics()[0].1, "on");
    rig.plugin.shutdown(&mut rig.host);
    assert_eq!(rig.plugin.diagnostics()[0].1, "off");
}
