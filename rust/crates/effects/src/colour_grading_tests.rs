use super::*;
use telescope_core::colour::sample_cube;
use telescope_core::testing::{scratch_dir, FakeHost};

struct Rig {
    plugin: ColourGradingPlugin,
    host: FakeHost,
}

impl Rig {
    /// A card with a phone paired and the saved settings loaded, as the window does at startup.
    fn new() -> Rig {
        let mut rig = Rig { plugin: ColourGradingPlugin::new(), host: FakeHost::new() };
        rig.plugin.setup(&mut rig.host);
        rig.plugin.set_config(&json!({})).unwrap();
        rig.event(Event::PhonesChanged { count: 1 });
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
        self.settle();
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
        self.settle();
    }

    /// Delivers the posts of a stage baked on its thread (until none is running), as the window would.
    fn settle(&mut self) {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while self.plugin.baking() && Instant::now() < deadline {
            let mine: Vec<(String, Value)> = {
                let mut posted = self.host.posted.lock().unwrap();
                let (mine, rest): (Vec<_>, Vec<_>) = posted.drain(..).partition(|(t, _)| t == "stage_baked" || t == "lut_read");
                *posted = rest;
                mine
            };
            for (token, data) in mine {
                self.plugin.on_event(&mut self.host, &Event::Posted { owner: "colour_grading".into(), token, data });
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Moves a slider on the Primaries page to a value in its own units.
    fn slider(&mut self, key: &str, value: f64) {
        let i = SLIDERS.iter().position(|d| d.key == key).unwrap();
        self.ui("slider-moved", vec![json!(i), json!(value)]);
    }

    /// What the pipeline was last told: Some(true) for a stage, Some(false) for none, None before any.
    fn published(&mut self) -> Option<bool> {
        self.settle();
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix("set_colour_stage colour_grading ").map(|r| r.starts_with("true")))
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    /// The load dialog or the export dialog answered.
    fn answer(&mut self, ok: bool, data: Value) {
        let (id, _) = self.plugin.asked.expect("a dialog was asked for");
        self.event(Event::RequestDone { request: id, ok, message: String::new(), data });
    }
}

fn write_lut(path: &Path, cube: Vec<f32>, size: usize) {
    write_cube(path, &cube, size, "").unwrap();
}

fn invert(size: usize) -> Vec<f32> {
    lattice(size).iter().map(|v| 1.0 - v).collect()
}

fn near(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
}

#[test]
fn it_is_an_optional_built_in_that_rides_in_presets_per_phone() {
    let info = ColourGradingPlugin::new().info();
    assert!(info.optional && info.in_presets && info.device_local && !info.needs_vision && info.category == "look");
}

#[test]
fn a_neutral_grade_puts_nothing_in_the_pipeline() {
    let mut r = Rig::new();
    assert_eq!(r.published(), Some(false));
    r.slider("saturation", 1.5);
    let stage = r.plugin.stage().expect("a stage");
    assert!(stage.table.is_none() && stage.size == 33, "saturation looks at every channel");
    assert!(sample_cube(&stage.rgb, 33, [1.0, 0.0, 0.0])[1].abs() < 0.01);
    assert_eq!(r.published(), Some(true));
    r.slider("saturation", 1.0);
    assert!(r.plugin.stage().is_none());
    assert_eq!(r.published(), Some(false));
}

#[test]
fn vibrance_bakes_into_the_lut_and_saves_only_when_set() {
    let mut r = Rig::new();
    r.slider("vibrance", 70.0);
    let stage = r.plugin.stage().expect("a stage");
    assert!(stage.table.is_none() && stage.size == 33, "vibrance looks at every channel");
    let want = Grade { vibrance: 70.0, ..Grade::default() };
    for c in [[0.5, 0.45, 0.55], [0.2, 0.5, 0.8], [0.8, 0.6, 0.5], [0.3, 0.3, 0.3]] {
        let (got, mut exact) = (sample_cube(&stage.rgb, 33, c), c);
        want.apply(&mut exact);
        assert!((0..3).all(|i| (got[i] - exact[i]).abs() < 0.02), "{c:?}: {got:?} vs {exact:?}");
    }
    assert_eq!(r.plugin.get_config()["vibrance"], json!(70.0));
    r.slider("vibrance", 0.0);
    assert!(r.plugin.stage().is_none() && r.plugin.get_config().get("vibrance").is_none());
    r.plugin.set_config(&json!({"vibrance": 250.0})).unwrap();
    assert_eq!(r.plugin.grade().vibrance, 100.0);
}

#[test]
fn switching_the_grade_off_takes_it_out_and_back() {
    let mut r = Rig::new();
    r.slider("contrast", 1.4);
    assert!(r.plugin.stage().unwrap().table.is_some(), "contrast is per channel: exact tables");
    r.ui("switch-toggled-by-hand", vec![json!(false)]);
    assert!(r.plugin.stage().is_none() && r.published() == Some(false) && r.plugin.get_config()["on"] == json!(false));
    r.ui("switch-toggled-by-hand", vec![json!(true)]);
    assert!(r.plugin.stage().is_some() && r.published() == Some(true));
}

#[test]
fn compare_draws_the_split_on_the_preview() {
    let mut r = Rig::new();
    assert_eq!(r.shown("compare"), json!(false));
    r.ui("compare-toggled", vec![json!(true)]);
    assert!(r.host.calls.contains(&"colour_split colour_grading Some(0.5)".to_string()));
    assert_eq!(r.shown("compare"), json!(true));
    r.ui("split-moved", vec![json!(30)]);
    assert!(r.host.calls.contains(&"colour_split colour_grading Some(0.3)".to_string()));
    assert_eq!(r.shown("split-text"), json!("30%"));
    r.ui("compare-toggled", vec![json!(false)]);
    assert_eq!(r.host.calls.last().unwrap(), "colour_split colour_grading None");
}

fn lift_wheel(r: &mut Rig, x: f64, y: f64) {
    r.plugin.wheels[0].set_value(x, y);
    r.plugin.wheel_moved(&mut r.host, 0);
}

#[test]
fn settings_survive_a_save_and_load() {
    let mut r = Rig::new();
    lift_wheel(&mut r, 0.2, -0.3);
    r.ui("master-moved", vec![json!(2), json!(1.3)]);
    r.slider("temperature", 25.0);
    r.slider("pivot", 0.5);
    r.ui("page-picked", vec![json!(1)]);
    r.ui("channel-picked", vec![json!(1)]);
    r.plugin.editor.points = vec![(0.0, 0.0), (0.4, 0.5), (1.0, 1.0)];
    r.plugin.curve_changed(&mut r.host);
    r.ui("curve-picked-by-hand", vec![json!(2)]);
    r.plugin.editor.points = vec![(0.0, 0.6), (0.5, 0.5)];
    r.plugin.curve_changed(&mut r.host);
    let cfg = r.plugin.get_config();
    assert_eq!(cfg["wheels"]["lift"], json!([0.2, -0.3]));
    assert_eq!(cfg["masters"]["gain"], json!(1.3));
    assert_eq!(cfg["temperature"], json!(25.0));
    assert_eq!(cfg["curves"]["r"], json!([[0.0, 0.0], [0.4, 0.5], [1.0, 1.0]]));
    assert_eq!(cfg["hue_hue"], json!([[0.0, 0.6], [0.5, 0.5]]));

    r.plugin.set_config(&json!({})).unwrap();
    r.event(Event::StreamStopped);
    assert!(r.plugin.grade().is_identity() && r.shown("slider-value").as_array().unwrap()[0] == json!(0.0));
    r.plugin.set_config(&cfg).unwrap();
    r.event(Event::StreamStopped);
    assert_eq!(r.plugin.get_config(), cfg);
    assert_eq!(r.shown("wheel-x")[0], json!(0.2));
    assert_eq!(r.shown("wheel-y")[0], json!(-0.3));
    assert_eq!(r.shown("master-value")[2], json!(1.3));
    assert_eq!(r.shown("master-text")[2], json!("1.30"));
    assert_eq!(r.shown("slider-text")[0], json!("+25"));
}

#[test]
fn the_default_settings_are_the_ones_python_saves() {
    let r = Rig::new();
    let cfg = r.plugin.get_config();
    let keys: Vec<&String> = cfg.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["on", "wheels", "masters", "curves", "hue_sat", "hue_hue", "lut", "lut_strength", "temperature", "tint", "contrast", "pivot",
                      "saturation", "shadows", "highlights"]);
    assert_eq!(cfg["wheels"], json!({"lift": [0.0, 0.0], "gamma": [0.0, 0.0], "gain": [0.0, 0.0], "offset": [0.0, 0.0]}));
    assert_eq!(cfg["masters"], json!({"lift": 0.0, "gamma": 0.0, "gain": 1.0, "offset": 0.0}));
    assert_eq!(cfg["curves"]["all"], json!([[0.0, 0.0], [1.0, 1.0]]));
    assert_eq!(cfg["hue_sat"][1], json!([1.0 / 6.0, 0.5]));
    assert_eq!((cfg["pivot"].clone(), cfg["saturation"].clone(), cfg["lut"].clone()), (json!(0.435), json!(1.0), json!("")));
}

#[test]
fn odd_saved_settings_load_as_something_sensible() {
    for cfg in [
        json!({"temperature": "hot", "contrast": 99, "wheels": {"lift": [5, 5]}, "masters": {"gain": null}}),
        json!({"curves": {"all": [[0.2, 0.2], [1, 1]], "r": "x"}, "hue_sat": [[0, 0.1], "bad", [0.5]], "wheels": [1, 2]}),
        json!({"lut": 7, "lut_strength": "x", "on": "maybe"}),
    ] {
        let mut r = Rig::new();
        r.plugin.set_config(&cfg).unwrap();
        let out = r.plugin.get_config();
        let temperature = out["temperature"].as_f64().unwrap();
        let contrast = out["contrast"].as_f64().unwrap();
        assert!((-100.0..=100.0).contains(&temperature) && (0.0..=2.0).contains(&contrast));
        assert!(out["curves"]["all"][0][0] == json!(0.0) && out["curves"]["all"].as_array().unwrap().last().unwrap()[0] == json!(1.0));
        let (x, y) = (out["wheels"]["lift"][0].as_f64().unwrap(), out["wheels"]["lift"][1].as_f64().unwrap());
        assert!(x * x + y * y <= 1.0 + 1e-6, "inside the wheel, where its puck can show it");
        // still a working grade
        let mut cube = lattice(3);
        r.plugin.grade().apply(&mut cube);
        assert!(cube.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn saved_hue_points_at_1_or_twice_come_back_as_one() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"hue_sat": [[0.0, 0.5], [0.5, 0.2], [1.0, 0.5], [0.5, 0.3]]})).unwrap();
    assert_eq!(r.plugin.get_config()["hue_sat"], json!([[0.0, 0.5], [0.5, 0.3]]));
}

#[test]
fn a_lut_file_loads_shows_its_strength_and_comes_off_again() {
    let mut r = Rig::new();
    let dir = scratch_dir("lut-load");
    let path = dir.join("Invert.cube");
    write_lut(&path, invert(3), 3);
    r.ui("page-picked", vec![json!(2)]);
    assert_eq!(r.shown("lut-loaded"), json!(false));
    r.ui("load-clicked", vec![]);
    assert!(r.host.calls.iter().any(|c| c == "pick_file Open Load a LUT"));
    r.answer(true, json!({"path": path.display().to_string(), "filter": "LUT files (*.cube)"}));
    assert_eq!((r.shown("lut-text"), r.shown("lut-loaded"), r.shown("lut-note")), (json!("Invert.cube"), json!(true), json!("")));
    let stage = r.plugin.stage().unwrap();
    assert!(near(sample_cube(&stage.rgb, 33, [0.2, 0.4, 0.6]), [0.8, 0.6, 0.4], 1e-4));
    r.ui("strength-moved", vec![json!(0)]);
    assert!(r.plugin.stage().is_none() && r.published() == Some(false));
    r.ui("strength-moved", vec![json!(100)]);
    assert_eq!(r.plugin.get_config()["lut"], json!(path.display().to_string()));
    r.ui("clear-clicked", vec![]);
    assert!(r.plugin.stage().is_none());
    assert_eq!(r.shown("lut-text"), json!(""));
    assert!(r.saves() >= 3);
}

#[test]
fn a_lut_that_cant_be_used_says_why() {
    let mut r = Rig::new();
    let dir = scratch_dir("lut-bad");
    let bad = dir.join("Big.cube");
    std::fs::write(&bad, "LUT_3D_SIZE 129\n").unwrap();
    r.plugin.load_lut(&mut r.host, &bad.display().to_string());
    assert!(r.shown("lut-note").as_str().unwrap().contains("129 points per side"));
    assert!(r.plugin.stage().is_none());
    r.plugin.load_lut(&mut r.host, &dir.join("gone.cube").display().to_string());
    assert_eq!(r.shown("lut-note"), json!("Telescope can't open that file."));
}

#[test]
fn a_preset_brings_its_lut_back_from_the_file() {
    let mut r = Rig::new();
    let dir = scratch_dir("lut-preset");
    let path = dir.join("Look.cube");
    write_lut(&path, lattice(3).iter().map(|v| v * 0.5).collect(), 3);
    r.plugin.apply_preset(&mut r.host, &json!({"lut": path.display().to_string(), "lut_strength": 0.5})).unwrap();
    assert!(r.plugin.grade().lut.is_some() && r.shown("strength") == json!(50.0) && r.published() == Some(true));
}

#[test]
fn switching_phones_publishes_the_lut_once_and_reads_the_file_once() {
    let mut r = Rig::new();
    let dir = scratch_dir("lut-switch");
    let path = dir.join("Look.cube");
    write_lut(&path, lattice(3).iter().map(|v| v * 0.5).collect(), 3);
    let saved = json!({"lut": path.display().to_string(), "saturation": 1.3});
    r.plugin.set_config(&saved).unwrap();
    r.event(Event::StreamStopped);
    let first = r.plugin.grade().lut.clone().unwrap();
    let before = r.host.colour_stages.len();
    // what switching phones does: the defaults, then the phone's settings
    r.plugin.set_config(&json!({})).unwrap();
    r.event(Event::StreamStopped);
    r.plugin.set_config(&saved).unwrap();
    r.event(Event::StreamStopped);
    r.event(Event::StreamStopped);
    assert_eq!(r.plugin.reads, 1, "the file was read once");
    assert!(Arc::ptr_eq(&first, r.plugin.grade().lut.as_ref().unwrap()));
    let published = &r.host.colour_stages[before..];
    assert_eq!(published.len(), 2, "no more than one publish per set of settings");
    assert!(published[0].1.is_none(), "the defaults take the stage off");
    // the published stage is the file's half brightness and the saved saturation
    let stage = published[1].1.as_ref().expect("the phone's grade");
    let mut want = lattice(stage.size as usize);
    Grade { lut: Some(first.clone()), saturation: 1.3, ..Grade::default() }.apply(&mut want);
    assert!(stage.rgb.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-6));
    assert!(near(sample_cube(&stage.rgb, stage.size as usize, [1.0, 1.0, 1.0]), [0.5, 0.5, 0.5], 0.01));
    // changed on disk: read again
    write_lut(&path, lattice(3).iter().map(|v| v * 0.25).collect(), 3);
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
    r.plugin.set_config(&saved).unwrap();
    assert_eq!(r.plugin.reads, 2);
    assert!(!Arc::ptr_eq(&first, r.plugin.grade().lut.as_ref().unwrap()));
}

fn exported(sizes: usize) -> (Rig, std::path::PathBuf) {
    let mut r = Rig::new();
    r.slider("saturation", 1.4);
    r.slider("temperature", -30.0);
    let dir = scratch_dir("export");
    r.ui("export-clicked", vec![]);
    let filters = ["33-point LUT (*.cube)", "65-point LUT (*.cube)"];
    r.answer(true, json!({"path": dir.join("My look").display().to_string(), "filter": filters[sizes]}));
    (r, dir)
}

#[test]
fn export_writes_the_grade_at_the_size_picked() {
    for (filter_index, size) in [(0, 33usize), (1, 65)] {
        let (r, dir) = exported(filter_index);
        assert!(r.host.calls.iter().any(|c| c == "pick_file Save Export the grade"));
        assert!(r.host.dialogs[0].start.ends_with("Telescope grade.cube"), "{}", r.host.dialogs[0].start);
        assert_eq!(r.host.dialogs[0].filters, ["33-point LUT (*.cube)", "65-point LUT (*.cube)"]);
        let back = read_cube(&dir.join("My look.cube")).unwrap();
        assert!(back.size() == size && back.title == "Telescope grade");
        let mut want = lattice(size);
        r.plugin.grade().apply(&mut want);
        let worst = back.cube.unwrap().iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "{worst}");
        assert_eq!(r.shown("export-note"), json!("Saved My look.cube."));
    }
}

#[test]
fn export_says_when_it_cant_write() {
    let mut r = Rig::new();
    let dir = scratch_dir("export-fail");
    r.ui("export-clicked", vec![]);
    r.answer(true, json!({"path": dir.join("missing").join("x.cube").display().to_string(), "filter": ""}));
    assert_eq!(r.shown("export-note"), json!("Telescope couldn't save the LUT there."));
    assert_eq!(r.shown("export-kind"), json!("warn"));
}

#[test]
fn export_asks_before_replacing_a_file_the_dialog_didnt_name() {
    let mut r = Rig::new();
    let dir = scratch_dir("export-replace");
    let target = dir.join("Mine.cube");
    std::fs::write(&target, "keep me").unwrap();
    r.ui("export-clicked", vec![]);
    r.answer(true, json!({"path": dir.join("Mine").display().to_string(), "filter": ""}));
    assert_eq!(r.shown("export-note"), json!("Mine.cube is already there. Replace it?"));
    r.ui("replace-answered", vec![json!(false)]);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
    r.ui("export-clicked", vec![]);
    r.answer(true, json!({"path": dir.join("Mine").display().to_string(), "filter": ""}));
    r.ui("replace-answered", vec![json!(true)]);
    assert!(std::fs::read_to_string(&target).unwrap().starts_with("TITLE"));
    // a name the dialog already ended in .cube was the dialog's own question to ask
    r.ui("export-clicked", vec![]);
    r.answer(true, json!({"path": target.display().to_string(), "filter": ""}));
    assert_eq!(r.shown("export-note"), json!("Saved Mine.cube."));
}

#[test]
fn a_cancelled_dialog_changes_nothing() {
    let mut r = Rig::new();
    r.ui("load-clicked", vec![]);
    r.answer(false, json!({}));
    assert_eq!(r.plugin.get_config()["lut"], json!(""));
    assert!(r.plugin.asked.is_none());
}

#[test]
fn reset_clears_the_grade_but_keeps_the_lut() {
    let mut r = Rig::new();
    let dir = scratch_dir("lut-reset");
    let path = dir.join("Look.cube");
    write_lut(&path, lattice(3).iter().map(|v| v * 0.5).collect(), 3);
    r.plugin.load_lut(&mut r.host, &path.display().to_string());
    r.slider("tint", 40.0);
    r.plugin.wheels[2].set_value(0.5, 0.5);
    r.plugin.wheel_moved(&mut r.host, 2);
    r.ui("reset-clicked", vec![]);
    assert!(r.plugin.grade().tint == 0.0 && r.plugin.grade().wheels[2] == (0.0, 0.0) && r.plugin.wheels[2].value() == (0.0, 0.0));
    assert!(r.plugin.grade().lut.is_some() && r.published() == Some(true));
}

#[test]
fn pages_show_one_at_a_time_and_the_channel_row_only_for_rgb() {
    let mut r = Rig::new();
    r.ui("page-picked", vec![json!(1)]);
    assert_eq!(r.shown("page"), json!(1));
    assert_eq!(r.shown("curve"), json!(0));
    r.ui("curve-picked-by-hand", vec![json!(1)]);
    assert_eq!(r.shown("curve"), json!(1));
    assert_eq!(r.plugin.editor.points, crate::grade::neutral_hue());
}

#[test]
fn shortcuts_reach_the_main_controls() {
    let mut r = Rig::new();
    let ids: Vec<String> = r.plugin.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["colour_grading.on", "colour_grading.compare", "colour_grading.temperature", "colour_grading.tint", "colour_grading.saturation", "colour_grading.vibrance", "colour_grading.contrast",
                    "colour_grading.pivot", "colour_grading.shadows", "colour_grading.highlights", "colour_grading.lift_master", "colour_grading.gamma_master",
                    "colour_grading.gain_master", "colour_grading.offset_master", "colour_grading.lut_strength"]);
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.temperature", &json!({"op": "up", "amount": 5})), Some("Temperature: +5".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.saturation", &json!({"op": "set", "value": 150})), Some("Saturation: 150%".into()));
    assert_eq!(r.plugin.grade().saturation, 1.5);
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.contrast", &json!({"op": "up", "amount": 0.05})), Some("Contrast: 1.05".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.contrast", &json!({"op": "reset"})), Some("Contrast: 1.00".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.shadows", &json!({"op": "set", "value": -20})), Some("Shadows: -20".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.pivot", &json!({"op": "set", "value": 0.25})), Some("Pivot: 0.250".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.gain_master", &json!({"op": "up", "amount": 0.1})), Some("Gain master: 1.10".into()));
    assert_eq!(r.plugin.grade().masters[2], 1.1);
    assert_eq!(r.shown("master-value")[2], json!(1.1));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.lift_master", &json!({"op": "down", "amount": 0.05})), Some("Lift master: -0.05".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.lift_master", &json!({"op": "reset"})), Some("Lift master: 0.00".into()));
    // the strength has no slider before a LUT is loaded
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.lut_strength", &json!({"op": "set", "value": 50})), None);
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.on", &json!({"mode": "off"})), Some("Colour grading: off".into()));
    assert!(!r.plugin.is_on());
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.compare", &json!({})), Some("Compare before and after: on".into()));
    // not on the Curves page, where the sliders are hidden
    r.ui("page-picked", vec![json!(1)]);
    assert_eq!(r.plugin.run_action(&mut r.host, "colour_grading.tint", &json!({"op": "up", "amount": 5})), None);
}

#[test]
fn a_held_key_flips_the_switch_and_puts_it_back() {
    let mut r = Rig::new();
    let hold = json!({"mode": "hold"});
    r.plugin.run_action(&mut r.host, "colour_grading.on", &hold);
    assert!(!r.plugin.is_on());
    r.plugin.release_action(&mut r.host, "colour_grading.on", &hold);
    assert!(r.plugin.is_on());
}

#[test]
fn the_shortcuts_have_nothing_to_do_before_a_phone_is_paired() {
    let mut p = ColourGradingPlugin::new();
    let mut host = FakeHost::new();
    assert_eq!(p.run_action(&mut host, "colour_grading.on", &json!({})), None);
}

#[test]
fn diagnostics_say_whether_a_grade_is_on() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.diagnostics(), [("Colour grading".to_string(), "neutral".to_string())]);
    r.slider("tint", 10.0);
    assert_eq!(r.plugin.diagnostics()[0].1, "on");
    r.ui("switch-toggled-by-hand", vec![json!(false)]);
    assert_eq!(r.plugin.diagnostics()[0].1, "off");
}

#[test]
fn shutting_down_takes_the_stage_and_split_out() {
    let mut r = Rig::new();
    r.slider("tint", 10.0);
    r.ui("compare-toggled", vec![json!(true)]);
    r.plugin.shutdown(&mut r.host);
    assert_eq!(r.published(), Some(false));
    assert_eq!(r.host.calls.last().unwrap(), "colour_split colour_grading None");
}

#[test]
fn a_wheel_moves_the_grade_through_the_card() {
    let mut r = Rig::new();
    r.ui("wheel-pressed", vec![json!(2)]);
    r.ui("wheel-dragged", vec![json!(2), json!(0.0), json!(-0.5), json!(false)]);
    r.ui("wheel-released", vec![json!(2)]);
    assert_eq!(r.plugin.grade().wheels[2], (0.0, 0.5));
    assert_eq!(r.published(), Some(true));
    r.ui("wheel-key", vec![json!(2), json!("home"), json!(false)]);
    assert_eq!(r.plugin.grade().wheels[2], (0.0, 0.0));
    r.ui("wheel-key", vec![json!(2), json!("right"), json!(true)]);
    r.ui("wheel-reset", vec![json!(2)]);
    assert!(r.plugin.grade().is_identity());
}

#[test]
fn a_curve_edit_goes_to_the_channel_picked() {
    let mut r = Rig::new();
    r.ui("page-picked", vec![json!(1)]);
    r.ui("channel-picked", vec![json!(2)]);
    // a click on the middle of the line adds a point, a drag moves it
    let (w, h) = (300.0, 200.0);
    let (left, top, aw, ah) = crate::editors::area(Kind::Tone, w, h);
    r.ui("curve-pressed", vec![json!(left + 0.5 * aw), json!(top + 0.5 * ah), json!(w), json!(h), json!(false)]);
    r.ui("curve-moved", vec![json!(left + 0.5 * aw), json!(top + 0.3 * ah), json!(w), json!(h)]);
    r.ui("curve-released", vec![]);
    let g = r.plugin.get_config();
    assert_eq!(g["curves"]["g"].as_array().unwrap().len(), 3);
    assert_eq!(g["curves"]["all"].as_array().unwrap().len(), 2);
    assert!((g["curves"]["g"][1][1].as_f64().unwrap() - 0.7).abs() < 0.02);
    assert!(r.plugin.stage().unwrap().table.is_some(), "a curve is per channel");
    r.ui("curve-key", vec![json!("home"), json!(false)]);
    assert!(r.plugin.grade().is_identity());
}

#[test]
fn a_big_grade_is_baked_for_the_pipeline_as_both_a_lattice_and_tables() {
    let mut r = Rig::new();
    r.slider("contrast", 1.3);
    let stage = r.plugin.stage().unwrap();
    assert_eq!(stage.rgb.len(), 33 * 33 * 33 * 3);
    let table = stage.table.unwrap();
    assert_eq!(table.len(), 256 * 3);
    // The table is the grade on a grey ramp, the lattice the grade on every colour
    let g = r.plugin.grade().eval([128.0 / 255.0; 3]);
    assert!(near([table[128 * 3], table[128 * 3 + 1], table[128 * 3 + 2]], g, 1e-6));
}

#[test]
fn the_stage_hands_the_pipeline_the_grade_itself_for_stages_that_come_after_another() {
    let mut r = Rig::new();
    r.slider("contrast", 2.0);
    let stage = r.plugin.stage().unwrap();
    let change = stage.eval.0.as_ref().expect("the grade itself");
    let mut at = vec![0.5f32, 0.25, 0.75];
    change(&mut at);
    let want = r.plugin.grade().eval([0.5, 0.25, 0.75]);
    assert_eq!(at, want.to_vec());
}

#[test]
#[ignore = "bench: cargo test --release -p telescope-effects --lib stage_build_cost -- --ignored --nocapture"]
fn stage_build_cost() {
    let mut p = ColourGradingPlugin::new();
    p.grade.temperature = 20.0;
    p.grade.saturation = 1.3;
    p.grade.shadows = 20.0;
    p.grade.vibrance = 15.0;
    p.grade.hue_sat = vec![(0.0, 0.0), (0.3, 0.2), (0.6, -0.1)];
    p.grade.curves[0] = vec![(0.0, 0.0), (0.4, 0.35), (1.0, 1.0)];
    let t = Instant::now();
    for _ in 0..20 {
        std::hint::black_box(p.stage());
    }
    println!("stage 33^3: {:.2} ms", t.elapsed().as_secs_f64() * 50.0);
    let t = Instant::now();
    for _ in 0..5 {
        let mut rgb = lattice(65);
        p.grade.apply(&mut rgb);
        std::hint::black_box(rgb);
    }
    println!("lattice 65^3: {:.2} ms", t.elapsed().as_secs_f64() * 200.0);
}

#[test]
fn a_drag_is_baked_on_a_thread_and_only_the_newest_grade_is_handed_over() {
    let mut r = Rig::new();
    let i = SLIDERS.iter().position(|d| d.key == "contrast").unwrap();
    for step in 1..=30 {
        // no settle between moves, as a fast drag
        r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "slider-moved".into(), args: vec![json!(i), json!(1.0 + step as f64 * 0.02)] });
    }
    assert_eq!(r.published(), Some(true));
    let handed = r.host.calls.iter().filter(|c| c.starts_with("set_colour_stage colour_grading true")).count();
    assert!(handed <= 3, "moves made while a bake ran are folded into one: {handed} stages handed over");
    assert!((r.plugin.grade().contrast - 1.6).abs() < 1e-9);
    let want = r.plugin.stage().unwrap();
    let last = r.host.colour_stages.last().and_then(|(_, s)| s.clone()).expect("the newest stage");
    assert_eq!((last.size, &last.rgb, &last.table), (want.size, &want.rgb, &want.table), "the stage handed over is the last grade's");
}

#[test]
fn switching_off_while_a_bake_runs_leaves_the_pipeline_with_no_stage() {
    let mut r = Rig::new();
    let i = SLIDERS.iter().position(|d| d.key == "contrast").unwrap();
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "slider-moved".into(), args: vec![json!(i), json!(1.5)] });
    r.plugin.on_ui(&mut r.host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(false)] });
    assert_eq!(r.published(), Some(false));
    r.event(Event::PhonesChanged { count: 1 });
    assert_eq!(r.published(), Some(false), "a bake that finished late doesn't put the grade back");
}

#[test]
fn a_saved_lut_isnt_read_while_the_effect_is_off_and_is_when_it_goes_on() {
    let mut r = Rig::new();
    let dir = scratch_dir("colour_lazy_lut");
    let path = dir.join("look.cube");
    write_lut(&path, invert(5), 5);
    r.plugin.set_config(&json!({"on": false, "lut": path.to_string_lossy()})).unwrap();
    assert_eq!(r.plugin.reads, 0, "off: no parse");
    assert_eq!(r.plugin.get_config()["lut"], json!(path.to_string_lossy()), "and the path is still saved");
    r.plugin.set_on(&mut r.host, true);
    r.settle();
    assert_eq!(r.plugin.reads, 1);
    assert!(r.plugin.grade().lut.is_some());
    assert_eq!(r.published(), Some(true));
}

#[test]
fn switching_on_reads_a_saved_lut_on_a_thread_and_publishes_when_it_lands() {
    let mut r = Rig::new();
    let dir = scratch_dir("colour_threaded_lut");
    let path = dir.join("look.cube");
    write_lut(&path, invert(5), 5);
    r.plugin.set_config(&json!({"on": false, "lut": path.to_string_lossy()})).unwrap();
    r.plugin.set_on(&mut r.host, true);
    // not read here: the thread has it, and nothing is handed over half-made
    assert!(r.plugin.baking() && r.plugin.grade().lut.is_none());
    assert_eq!(r.host.calls.iter().filter(|c| c.starts_with("set_colour_stage colour_grading true")).count(), 0);
    r.settle();
    assert!(r.plugin.grade().lut.is_some());
    assert_eq!(r.published(), Some(true));
    assert_eq!(r.plugin.reads, 1);
}

#[test]
fn a_lut_read_that_lands_after_the_effect_went_off_again_is_dropped() {
    let mut r = Rig::new();
    let dir = scratch_dir("colour_superseded_lut");
    let path = dir.join("look.cube");
    write_lut(&path, invert(5), 5);
    r.plugin.set_config(&json!({"on": false, "lut": path.to_string_lossy()})).unwrap();
    r.plugin.set_on(&mut r.host, true);
    r.plugin.set_on(&mut r.host, false);
    r.settle();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let late: Vec<(String, Value)> = std::mem::take(&mut *r.host.posted.lock().unwrap());
    for (token, data) in late {
        r.plugin.on_event(&mut r.host, &Event::Posted { owner: "colour_grading".into(), token, data });
    }
    assert!(r.plugin.grade().lut.is_none(), "the effect is off: the file wasn't taken in");
    assert_ne!(r.published(), Some(true));
    // and the next switching on reads it again
    r.plugin.set_on(&mut r.host, true);
    r.settle();
    assert!(r.plugin.grade().lut.is_some());
}
