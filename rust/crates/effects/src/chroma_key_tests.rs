use super::*;
use telescope_core::host::Pick;
use telescope_core::media::{Media, StagedStep};
use telescope_core::testing::FakeHost;

const GREEN: [u8; 3] = [60, 180, 50];
const BLUE: [u8; 3] = [190, 80, 20];
/// B, G, R
const SKIN: [u8; 3] = [120, 150, 210];

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

/// A frame of one B, G, R colour.
fn flat(w: u32, h: u32, bgr: [u8; 3]) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, [bgr[2], bgr[1], bgr[0], 255].repeat((w * h) as usize))
}

struct Rig {
    plugin: ChromaKeyPlugin,
    host: FakeHost,
}

impl Rig {
    fn new(can_pick: bool) -> Rig {
        let mut rig = Rig { plugin: ChromaKeyPlugin::new(), host: FakeHost::new() };
        rig.host.can_pick = can_pick;
        rig.plugin.setup(&mut rig.host);
        // the card is dimmed until a phone is paired
        rig.event(Event::PhonesChanged { count: 1 });
        rig.plugin.set_config(&json!({})).unwrap();
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    /// The newest value the card was given for a property.
    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn config(&self) -> Value {
        self.plugin.get_config()
    }

    /// What the stream does with a frame while a pick runs: it only looks.
    fn look(&mut self, mut frame: Frame) {
        let Some(FrameStep::Cpu(mut tap)) = self.plugin.frame_step() else { panic!("a CPU step that looks") };
        tap.process(&mut frame, &NoContext);
    }

    fn picking(&mut self) {
        self.event(Event::PreviewPickChanged { owner: NAME.into() });
    }

    fn click(&mut self, at: [f32; 2]) {
        let pick = Pick { kind: PickKind::Point, shown: vec![at], full: vec![at], in_picture: vec![at], picture: None };
        self.event(Event::PreviewPicked { owner: NAME.into(), pick: Some(pick) });
    }
}

fn shader(plugin: &mut ChromaKeyPlugin) -> ShaderStep {
    match plugin.frame_step() {
        Some(FrameStep::Shader(s)) => s,
        other => panic!("a shader step, got {other:?}"),
    }
}

/// Whether this machine has a graphics card the key's shader can run on.
fn gpu_here() -> bool {
    let here = telescope_media::gpu::Gpu::shared().is_some();
    if !here {
        eprintln!("no graphics card here: the shader checks are skipped");
    }
    here
}

/// A step through the app's own frame pipeline.
fn through(step: FrameStep, frame: Frame) -> Frame {
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Background, step }]);
    pipeline.process(frame, &NoContext)
}

/// A pixel of a frame as B, G, R.
fn at(frame: &Frame, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * frame.width + x) * 4) as usize;
    [frame.data[i + 2], frame.data[i + 1], frame.data[i]]
}

/// What a key leaves of a frame: how much of each pixel stays, and the picture over a black backdrop.
struct Matte {
    width: u32,
    alpha: Vec<u8>,
    over_black: Frame,
}

impl Matte {
    fn a(&self, x: u32, y: u32) -> u8 {
        self.alpha[(y * self.width + x) as usize]
    }

    /// The largest and smallest alpha of a block of the picture.
    fn range(&self, ys: std::ops::Range<u32>, xs: std::ops::Range<u32>) -> (u8, u8) {
        let all: Vec<u8> = ys.flat_map(|y| xs.clone().map(move |x| (x, y))).map(|(x, y)| self.a(x, y)).collect();
        (*all.iter().max().unwrap(), *all.iter().min().unwrap())
    }

    /// The colour with the spill out, where nothing of the backdrop shows through.
    fn clean(&self, x: u32, y: u32) -> [u8; 3] {
        assert_eq!(self.a(x, y), 255, "all of the pixel has to stay for its colour to show");
        at(&self.over_black, x, y)
    }
}

/// matte() read off the production shader: the gap between a white and a black backdrop is what didn't stay.
fn matte(frame: &Frame, cfg: Value) -> Matte {
    let over = |colour: [u8; 3]| {
        let mut p = ChromaKeyPlugin::new();
        let mut cfg = cfg.clone();
        cfg["on"] = json!(true);
        cfg["colour"] = json!(colour);
        p.set_config(&cfg).unwrap();
        through(p.frame_step().expect("a key"), frame.clone())
    };
    let (white, black) = (over([255; 3]), over([0; 3]));
    let alpha = (0..frame.height).flat_map(|y| (0..frame.width).map(move |x| (x, y))).map(|(x, y)| 255 - (at(&white, x, y)[0] - at(&black, x, y)[0])).collect();
    Matte { width: frame.width, alpha, over_black: black }
}

/// A green screen with a skin-coloured block in the middle.
fn scene() -> Frame {
    let (w, h) = (64usize, 36usize);
    let mut data = flat(w as u32, h as u32, GREEN).data.to_vec();
    for y in 10..26 {
        for x in 20..44 {
            data[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&[SKIN[2], SKIN[1], SKIN[0]]);
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
}

#[test]
fn the_screen_goes_and_the_person_stays() {
    if !gpu_here() {
        return;
    }
    let m = matte(&scene(), json!({"tolerance": 40, "softness": 30, "spill": 60}));
    assert!(m.range(2..6, 2..10).0 < 10, "screen");
    assert!(m.range(14..22, 26..38).1 > 245, "person");
}

#[test]
fn tolerance_decides_how_far_from_the_screen_still_goes() {
    if !gpu_here() {
        return;
    }
    // a slightly different green, as a crease or shadow on the screen would be
    let near = flat(4, 4, [80, 170, 70]);
    assert!(matte(&near, json!({"tolerance": 0, "softness": 0, "spill": 0})).range(0..4, 0..4).1 > 200);
    assert!(matte(&near, json!({"tolerance": 90, "softness": 0, "spill": 0})).range(0..4, 0..4).0 < 30);
}

#[test]
fn softness_makes_a_ramp_instead_of_a_cliff() {
    let (cb, cr) = chroma_of(GREEN);
    let hard = key_tables(GREEN, 40, 0, 0).alpha;
    let soft = key_tables(GREEN, 40, 100, 0).alpha;
    let ramp = |t: &[u8]| t.iter().filter(|&&a| a > 10 && a < 245).count();
    assert!(ramp(&soft) > 3 * ramp(&hard));
    let at = ((cr.round() as i32 + 128) * 256 + cb.round() as i32 + 128) as usize;
    assert_eq!((hard[at], soft[at]), (0, 0));
}

#[test]
fn spill_takes_green_out_of_a_green_tinged_edge_but_leaves_other_colours() {
    if !gpu_here() {
        return;
    }
    // skin with green light on it
    let tinged = flat(4, 4, [110, 170, 140]);
    let kept = matte(&tinged, json!({"tolerance": 10, "softness": 10, "spill": 0})).clean(0, 0);
    let clean = matte(&tinged, json!({"tolerance": 10, "softness": 10, "spill": 100})).clean(0, 0);
    let green_in = |px: [u8; 3]| px[1] as i32 - (px[0] as i32 + px[2] as i32) / 2;
    assert!(green_in(clean) < green_in(kept) - 10, "{clean:?} against {kept:?}");
    // skin, red, blue: nowhere near green
    for colour in [SKIN, [40, 40, 220], [220, 60, 40]] {
        let clean = matte(&flat(4, 4, colour), json!({"tolerance": 40, "softness": 30, "spill": 100})).clean(0, 0);
        assert!((0..3).all(|c| (clean[c] as i32 - colour[c] as i32).abs() <= 6), "{colour:?} became {clean:?}");
    }
}

#[test]
fn a_blue_screen_keys_blue_and_keeps_green() {
    if !gpu_here() {
        return;
    }
    let mut data = flat(8, 8, BLUE).data.to_vec();
    for y in 0..8 {
        for x in 4..8 {
            data[(y * 8 + x) * 4..(y * 8 + x) * 4 + 3].copy_from_slice(&[GREEN[2], GREEN[1], GREEN[0]]);
        }
    }
    let m = matte(&Frame::new(8, 8, PixelFormat::Rgba, data), json!({"screen": "blue", "tolerance": 40, "softness": 30, "spill": 60}));
    assert!(m.range(0..8, 0..3).0 < 10 && m.range(0..8, 5..8).1 > 245);
}

#[test]
fn a_picked_colour_is_the_median_around_the_click() {
    let mut img = vec![];
    for _ in 0..40 * 40 {
        img.extend_from_slice(&[GREEN[2], GREEN[1], GREEN[0], 255]);
    }
    // one bright speck doesn't decide it
    let at = (20 * 40 + 20) * 4;
    img[at..at + 4].copy_from_slice(&[255; 4]);
    let frame = Frame::new(40, 40, PixelFormat::Rgba, img);
    assert_eq!(sample(&frame, 0.5, 0.5), GREEN);
    assert_eq!(sample(&frame, 1.0, 0.0), GREEN);
    assert!(keyable(GREEN) && !keyable([128, 128, 128]) && !keyable([250, 250, 250]));
}

#[test]
fn the_tables_are_the_ones_opencv_makes() {
    // chroma_of is cv2.cvtColor's own fixed-point maths, checked against what Python printed
    assert_eq!((chroma_of(GREEN), chroma_of(BLUE), chroma_of([160, 40, 150])), ((-38.0, -55.0), (65.0, -39.0), (41.0, 45.0)));
    let t = key_tables(GREEN, 40, 30, 60);
    let sum = |v: &[u8]| v.iter().map(|&x| x as u64).sum::<u64>();
    assert_eq!((sum(&t.alpha), sum(&t.cr), sum(&t.cb)), (15731621, 9011248, 8808676));
    assert_eq!((t.alpha.len(), t.cr.len(), t.cb.len()), (65536, 65536, 65536));
    let texture = t.texture();
    assert_eq!((texture.width, texture.height), (256, 256));
    assert_eq!(&texture.data[4 * 777..4 * 777 + 4], &[t.alpha[777], t.cr[777], t.cb[777], 255]);
}

#[test]
fn a_pick_starts_where_the_card_says_and_keys_that_colour() {
    let mut r = Rig::new(true);
    let purple = [160, 40, 150];
    r.picking();
    r.look(flat(64, 36, purple));
    // nothing picked yet: Picked stays what it was until one comes in
    r.ui("screen-picked", vec![json!(2)]);
    assert!(r.host.calls.iter().any(|c| c == "pick_on_preview chroma_key Point"));
    assert_eq!(r.config()["picked"], Value::Null);
    assert_eq!(r.config()["screen"], json!("green"));
    assert_eq!(r.shown("screen"), json!(0), "nothing picked yet");
    r.click([0.5, 0.5]);
    assert_eq!(r.config()["picked"], json!(purple));
    assert_eq!((r.config()["screen"].clone(), r.shown("screen")), (json!("picked"), json!(2)));
    r.event(Event::PreviewPickChanged { owner: String::new() });
    r.ui("switch-toggled-by-hand", vec![json!(true)]);
    let step = shader(&mut r.plugin);
    if gpu_here() {
        assert_eq!(at(&through(FrameStep::Shader(step), flat(64, 36, purple)), 5, 5), DEFAULT_COLOUR);
    }
}

#[test]
fn a_pick_reads_the_keys_own_input_not_the_cleaned_up_preview() {
    let mut r = Rig::new(true);
    r.host.stream_frames = vec![telescope_core::frame::StreamFrame { source_id: String::new(), name: String::new(), frame: flat(40, 20, [255, 255, 255]) }];
    // the screen, where the click lands once mapped back past Desk mode's warp
    let mut data = flat(64, 36, [128, 128, 128]).data.to_vec();
    for y in 0..36 {
        for x in 0..20 {
            let i = (y * 64 + x) * 4;
            data[i..i + 4].copy_from_slice(&[150, 40, 160, 255]);
        }
    }
    r.picking();
    r.look(Frame::new(64, 36, PixelFormat::Rgba, data));
    // on the preview Desk's Black and white turned it all white
    let pick = Pick { kind: PickKind::Point, shown: vec![[0.2, 0.5]], full: vec![[0.2, 0.5]], in_picture: vec![[0.9, 0.5]], picture: Some(flat(40, 20, [255, 255, 255])) };
    r.event(Event::PreviewPicked { owner: NAME.into(), pick: Some(pick) });
    assert_eq!(r.config()["picked"], json!([160, 40, 150]));
}

#[test]
fn without_a_frame_yet_a_pick_reads_the_preview() {
    let mut r = Rig::new(true);
    let mut data = flat(40, 20, [128, 128, 128]).data.to_vec();
    for y in 0..20 {
        for x in 30..40 {
            let i = (y * 40 + x) * 4;
            data[i..i + 4].copy_from_slice(&[150, 40, 160, 255]);
        }
    }
    // A newer frame is in the store by now: the one clicked on is what counts
    r.host.stream_frames = vec![telescope_core::frame::StreamFrame { source_id: String::new(), name: String::new(), frame: flat(40, 20, [128, 128, 128]) }];
    r.picking();
    let shown = Some(Frame::new(40, 20, PixelFormat::Rgba, data));
    let pick = Pick { kind: PickKind::Point, shown: vec![[0.2, 0.5]], full: vec![[0.2, 0.5]], in_picture: vec![[0.9, 0.5]], picture: shown };
    r.event(Event::PreviewPicked { owner: NAME.into(), pick: Some(pick) });
    assert_eq!(r.config()["picked"], json!([160, 40, 150]));
}

#[test]
fn keying_waits_while_its_own_pick_runs() {
    let mut r = Rig::new(true);
    r.plugin.set_config(&json!({"on": true})).unwrap();
    let frame = scene();
    assert!(matches!(r.plugin.frame_step(), Some(FrameStep::Shader(_))));
    r.event(Event::PreviewPickChanged { owner: NAME.into() });
    let Some(FrameStep::Cpu(mut tap)) = r.plugin.frame_step() else { panic!("a step that only looks") };
    let mut seen = frame.clone();
    tap.process(&mut seen, &NoContext);
    assert_eq!(seen.data, frame.data, "the picture goes through untouched");
    assert!(r.host.calls.iter().filter(|c| *c == "refresh_frame_step").count() >= 1);
    r.event(Event::PreviewPickChanged { owner: String::new() });
    let keyed = r.plugin.frame_step().expect("the key is back");
    if gpu_here() {
        assert_ne!(through(keyed, frame.clone()).data, frame.data);
    }
    // somebody else's pick isn't ours to wait for
    r.event(Event::PreviewPickChanged { owner: "desk_mode".into() });
    assert!(matches!(r.plugin.frame_step(), Some(FrameStep::Shader(_))));
}

#[test]
fn a_grey_pick_is_refused() {
    let mut r = Rig::new(true);
    r.picking();
    r.look(flat(64, 36, [128, 128, 128]));
    r.ui("screen-picked", vec![json!(2)]);
    r.click([0.5, 0.5]);
    assert_eq!(r.config()["picked"], Value::Null);
    assert_eq!(r.shown("status"), json!("No colour there"));
    assert!(r.shown("status-detail").as_str().unwrap().contains("hardly any colour"));
}

#[test]
fn without_a_stream_the_card_says_to_start_one() {
    let mut r = Rig::new(false);
    r.ui("screen-picked", vec![json!(2)]);
    assert_eq!(r.shown("status"), json!("Not streaming"));
    assert!(r.shown("status-detail").as_str().unwrap().contains("Start streaming"));
    // picking again, with a colour picked before
    r.plugin.set_config(&json!({"picked": [200, 40, 160]})).unwrap();
    r.event(Event::StreamStopped);
    r.ui("screen-picked", vec![json!(2)]);
    assert_eq!(r.shown("status"), json!("Not streaming"));
    assert!(r.shown("status-detail").as_str().unwrap().contains("Start streaming"));
    assert_eq!(r.shown("status-kind"), json!("status_warn"));
}

#[test]
fn settings_round_trip_and_bad_values_fall_back() {
    let mut r = Rig::new(true);
    let cfg = json!({"on": true, "screen": "picked", "picked": [160, 40, 150], "tolerance": 70, "softness": 10, "spill": 0,
                     "background": "colour", "colour": [1, 2, 3], "picture": ""});
    r.plugin.set_config(&cfg).unwrap();
    assert_eq!(r.config().to_string(), cfg.to_string(), "the same keys in the same order");
    r.plugin.set_config(&json!({"screen": "picked", "picked": [128, 128, 128], "tolerance": 500, "colour": "red", "background": "video"})).unwrap();
    let got = r.config();
    assert_eq!((got["screen"].clone(), got["picked"].clone(), got["tolerance"].clone(), got["background"].clone()), (json!("green"), Value::Null, json!(100), json!("colour")));
    assert_eq!(got["colour"], json!([40, 34, 30]));
    r.plugin.set_config(&json!({"picked": [1.5, 2, 3], "colour": [1, 2, 300], "on": 1, "softness": "12", "spill": null})).unwrap();
    let got = r.config();
    assert_eq!((got["picked"].clone(), got["colour"].clone(), got["on"].clone(), got["softness"].clone(), got["spill"].clone()), (Value::Null, json!([40, 34, 30]), json!(false), json!(12), json!(60)));
    r.plugin.set_config(&json!([1, 2])).unwrap();
    assert_eq!(r.config()["tolerance"], json!(40));
}

#[test]
fn a_stream_behind_the_panels_keeps_its_key() {
    let mut r = Rig::new(true);
    assert!(r.plugin.frame_step().is_none());
    r.plugin.set_config(&json!({"on": true, "colour": [9, 9, 9]})).unwrap();
    let step = shader(&mut r.plugin);
    r.plugin.set_config(&json!({})).unwrap();
    let nine = 9.0 / 255.0;
    assert_eq!(step.uniforms["backdrop"], json!([nine as f32, nine as f32, nine as f32, 0.0]));
    assert!(r.plugin.frame_step().is_none());
    if gpu_here() {
        assert_eq!(at(&through(FrameStep::Shader(step), scene()), 2, 2), [9, 9, 9]);
    }
}

#[test]
fn the_backdrop_is_a_colour_or_the_picture_covering_the_frame() {
    let tables = json!({"tolerance": 40, "softness": 30, "spill": 60, "on": true});
    let mut cfg = tables.clone();
    cfg["colour"] = json!([10, 20, 30]);
    let mut p = ChromaKeyPlugin::new();
    p.set_config(&cfg).unwrap();
    let solid = shader(&mut p);
    let picture = flat(10, 10, [0, 0, 200]);
    let given = picture.clone();
    let mut p = ChromaKeyPlugin::with_reader(Arc::new(move |path| if path == "/pics/red.png" { Ok(given.clone()) } else { Err("no".into()) }));
    let mut cfg = tables.clone();
    cfg["background"] = json!("picture");
    cfg["picture"] = json!("/pics/red.png");
    cfg["colour"] = json!([10, 20, 30]);
    p.set_config(&cfg).unwrap();
    let step = shader(&mut p);
    assert_eq!(step.uniforms["backdrop"][3], json!(1.0));
    assert_eq!(step.uniforms["picture"], json!([10.0, 10.0]));
    assert_eq!(step.textures.iter().map(|(n, f)| (n.as_str(), f.width)).collect::<Vec<_>>(), vec![("keys", 256), ("picture", 10)]);
    if gpu_here() {
        let out = through(FrameStep::Shader(solid), scene());
        assert_eq!(at(&out, 2, 2), [10, 20, 30]);
        assert!((0..3).all(|c| (at(&out, 32, 18)[c] as i32 - SKIN[c] as i32).abs() <= 6), "{:?}", at(&out, 32, 18));
        assert_eq!(at(&through(FrameStep::Shader(step), scene()), 2, 2), [0, 0, 200]);
    }
    // switched off, or a picture that won't open: the colour
    p.set_config(&json!({"on": true, "background": "picture", "picture": "/pics/gone.png", "colour": [9, 9, 9]})).unwrap();
    assert_eq!(shader(&mut p).uniforms["backdrop"][3], json!(0.0));
    p.set_config(&json!({"on": false})).unwrap();
    assert!(p.frame_step().is_none());
}

#[test]
fn a_picture_that_wont_open_or_is_missing_says_so() {
    let mut r = Rig::new(true);
    r.plugin.set_config(&json!({"on": true, "background": "picture", "picture": "/pics/gone.png"})).unwrap();
    r.event(Event::PhonesChanged { count: 1 });
    assert_eq!(r.shown("picture-problem"), json!("Telescope can't open that picture."));
    assert_eq!(r.shown("picture-text"), json!("gone.png"));
    r.plugin.set_config(&json!({"on": true, "background": "picture"})).unwrap();
    r.event(Event::PhonesChanged { count: 1 });
    assert_eq!((r.shown("picture-text"), r.shown("picture-problem"), r.shown("has-picture")), (json!(""), json!(""), json!(false)));
    r.plugin.set_config(&json!({"on": false, "background": "picture"})).unwrap();
    r.event(Event::PhonesChanged { count: 1 });
    assert_eq!(r.shown("status"), json!(""));
}

#[test]
fn the_shortcuts_switch_it_pick_the_screen_and_move_the_tolerance() {
    let mut r = Rig::new(true);
    let run = |r: &mut Rig, id: &str, p: Value| r.plugin.run_action(&mut r.host, id, &p);
    assert_eq!(run(&mut r, "chroma_key.on", json!({"mode": "on"})).as_deref(), Some("Chroma key: on"));
    assert!(r.plugin.is_on());
    assert_eq!(run(&mut r, "chroma_key.screen", json!({"op": "next"})).as_deref(), Some("Chroma key screen: Blue"));
    assert_eq!(r.config()["screen"], json!("blue"));
    // Picked with nothing picked yet starts a pick and stays on blue
    assert_eq!(run(&mut r, "chroma_key.screen", json!({"op": "set", "option": "picked"})).as_deref(), Some("Chroma key screen: Picked"));
    assert_eq!(r.config()["screen"], json!("blue"));
    assert_eq!(run(&mut r, "chroma_key.tolerance", json!({"op": "up", "amount": 5.0})).as_deref(), Some("Chroma key tolerance: 45"));
    assert_eq!(run(&mut r, "chroma_key.tolerance", json!({"op": "reset"})).as_deref(), Some("Chroma key tolerance: 40"));
    let ids: Vec<String> = r.plugin.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["chroma_key.on", "chroma_key.screen", "chroma_key.tolerance"]);
    // a card nobody paired a phone with has nothing for a key to do
    let mut idle = ChromaKeyPlugin::new();
    assert_eq!(idle.run_action(&mut r.host, "chroma_key.on", &json!({})), None);
}

#[test]
fn it_says_what_it_is() {
    let info = ChromaKeyPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str()), ("chroma_key", "look", "eyedropper"));
    assert!(info.optional && info.in_presets && info.device_local);
    assert_eq!(info.frame_stage, FrameStage::Background);
    assert_eq!(ChromaKeyPlugin::new().card().unwrap().region, "right");
    assert_eq!(ChromaKeyPlugin::new().diagnostics(), vec![("Chroma key".to_string(), "off, green screen, colour behind".to_string())]);
}

#[test]
fn a_saturated_key_colour_is_clamped_to_a_byte_as_opencv_does() {
    // pure red would reach Cr 256 unclamped; OpenCV saturates it to 255, so it centres at 127
    assert_eq!((chroma_of([0, 0, 255]), chroma_of([255, 0, 0])), ((-43.0, 127.0), (127.0, -21.0)));
    // and the tables around it are the ones Python makes
    let sum = |v: &[u8]| v.iter().map(|&x| x as u64).sum::<u64>();
    let red = key_tables([0, 0, 255], 40, 30, 60);
    assert_eq!((sum(&red.alpha), sum(&red.cr), sum(&red.cb)), (14722651, 7659545, 8591522));
    let blue = key_tables([255, 0, 0], 40, 30, 60);
    assert_eq!((sum(&blue.alpha), sum(&blue.cr), sum(&blue.cb)), (14877747, 8471525, 7655053));
}

#[test]
fn the_pad_and_strip_set_the_stored_colour_as_before() {
    let mut r = Rig::new(false);
    assert_eq!(r.config()["colour"], json!(DEFAULT_COLOUR));
    // full red at the top right of the pad, red's hue
    r.ui("colour-moved", vec![json!(0.0), json!(1.0), json!(1.0)]);
    assert_eq!(r.config()["colour"], json!([0, 0, 255]), "stored as B, G, R, whole numbers");
    assert_eq!((r.shown("colour-h"), r.shown("colour-s"), r.shown("colour-v")), (json!(0.0), json!(1.0), json!(1.0)));
    assert_eq!((r.shown("backdrop-colour"), r.shown("colour-hex"), r.shown("channel-value")), (json!("#ff0000"), json!("#ff0000"), json!([255, 0, 0])));
    // dragging down to black keeps the hue the person had, so coming back up is the same colour
    r.ui("colour-moved", vec![json!(0.25), json!(0.5), json!(0.0)]);
    assert_eq!(r.config()["colour"], json!([0, 0, 0]));
    assert_eq!((r.shown("colour-h"), r.shown("colour-s")), (json!(0.25), json!(0.5)));
    r.ui("colour-moved", vec![json!(0.25), json!(0.5), json!(1.0)]);
    assert_eq!(r.config()["colour"], json!([128, 255, 191]));
    // the card changed the setting, so it is saved and the stream rebuilt
    assert!(r.host.calls.iter().any(|c| c == "schedule_save") && r.host.calls.iter().any(|c| c == "refresh_frame_step"));
    // a colour that came from the saved settings is what it is on the pad
    r.plugin.set_config(&json!({"colour": [0, 255, 0]})).unwrap();
    r.ui("hex-typed", vec![json!("#00ff00")]);
    assert_eq!((r.shown("colour-h"), r.shown("colour-s"), r.shown("colour-v")), (json!(1.0f32 / 3.0), json!(1.0), json!(1.0)));
}

#[test]
fn typed_values_set_the_colour_and_nonsense_leaves_it() {
    let mut r = Rig::new(false);
    r.ui("hex-typed", vec![json!(" #FF8800 ")]);
    assert_eq!(r.config()["colour"], json!([0, 136, 255]));
    r.ui("hex-typed", vec![json!("not a colour")]);
    assert_eq!(r.config()["colour"], json!([0, 136, 255]), "a typo changes nothing");
    assert_eq!(r.shown("colour-hex"), json!("#ff8800"), "and the box is told what it still is, with its #");
    r.ui("channel-typed", vec![json!(2), json!("12")]);
    r.ui("channel-typed", vec![json!(1), json!("999")]);
    r.ui("channel-typed", vec![json!(0), json!("-5")]);
    assert_eq!(r.config()["colour"], json!([12, 255, 0]), "blue 12, green and red held to 0 to 255, stored as B, G, R");
    r.ui("channel-typed", vec![json!(0), json!("lots")]);
    assert_eq!(r.config()["colour"], json!([12, 255, 0]));
}

#[test]
fn the_pipette_picks_the_backdrop_from_the_preview_without_pausing_the_key() {
    let mut r = Rig::new(true);
    r.ui("pipette-clicked", vec![]);
    assert!(r.host.calls.iter().any(|c| c == "pick_on_preview chroma_key.backdrop Point"));
    // the key keeps working while it runs: only the screen's own pick steps it aside
    r.event(Event::PreviewPickChanged { owner: "chroma_key.backdrop".into() });
    r.ui("switch-toggled-by-hand", vec![json!(true)]);
    assert!(matches!(r.plugin.frame_step(), Some(FrameStep::Shader(_))));
    // the colour is the one on the picture the preview showed
    let pick = Pick { kind: PickKind::Point, shown: vec![[0.5, 0.5]], full: vec![[0.5, 0.5]], in_picture: vec![[0.5, 0.5]], picture: Some(flat(32, 18, [20, 90, 200])) };
    r.event(Event::PreviewPicked { owner: "chroma_key.backdrop".into(), pick: Some(pick) });
    assert_eq!(r.config()["colour"], json!([20, 90, 200]));
    assert_eq!(r.config()["picked"], Value::Null, "and the screen's own pick is untouched");
    // without a preview it says why
    let mut r = Rig::new(false);
    r.ui("pipette-clicked", vec![]);
    assert_eq!(r.shown("status"), json!("Not streaming"));
    assert!(r.shown("status-detail").as_str().unwrap().contains("Start streaming"));
}

#[test]
fn hex_is_read_with_or_without_the_hash_short_or_long_and_alpha_is_dropped() {
    use crate::shared::parse_hex;
    for (typed, want) in [
        ("#ff8800", Some([255, 136, 0])),
        ("ff8800", Some([255, 136, 0])),
        ("  #FF8800  ", Some([255, 136, 0])),
        ("#f80", Some([255, 136, 0])),
        ("f80", Some([255, 136, 0])),
        ("#ff8800cc", Some([255, 136, 0])),
        ("ff880000", Some([255, 136, 0])),
        ("#f80c", Some([255, 136, 0])),
        ("f80c", Some([255, 136, 0])),
    ] {
        assert_eq!(parse_hex(typed), want, "{typed:?}");
    }
    for typed in ["", "#", "##ff8800", "#ff880", "#ff88000", "#ff8800cc1", "#gg0000", "#ff 88 00", "#ff8800;", "\u{ff03}ff8800", "ff88\u{e9}0", "red"] {
        assert_eq!(parse_hex(typed), None, "{typed:?}");
    }
}

#[test]
fn a_long_hex_with_alpha_sets_the_colour_and_the_box_shows_it_as_six_digits() {
    let mut r = Rig::new(false);
    r.ui("hex-typed", vec![json!("#12345678")]);
    assert_eq!(r.config()["colour"], json!([0x56, 0x34, 0x12]), "stored as B, G, R");
    assert_eq!(r.shown("colour-hex"), json!("#123456"));
    r.ui("hex-typed", vec![json!("#12345")]);
    assert_eq!(r.shown("colour-hex"), json!("#123456"), "five digits is a typo: it reverts");
}
