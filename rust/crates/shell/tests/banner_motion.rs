// The banners' motion: a strip grows open and shuts again with everything under it following, a banner that is dismissed mid-way
// closes from where it is, several banners open and close each on its own while the others slide, and a banner that says something
// new cross-fades and moves its height instead of closing and opening. BANNER_SHOTS=dir (or MOTION_SHOTS) draws the film strips.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint::{Model, ModelRc, VecModel};
use slint_interpreter::{Struct, Value};
use std::rc::Rc;
use telescope_shell::offscreen;

const DOC: &str = r#"
import { BannerArea, BannerData } from "shell/frame.slint";
import { Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 900px;
    height: 300px;
    background: Theme.ground;
    in property <[BannerData]> banners;
    out property <length> tail-y: tail.y;
    out property <int> closed-count;
    out property <string> closed-key;
    area := BannerArea {
        x: 0;
        y: 0;
        width: root.width;
        height: self.preferred-height;
        banners: root.banners;
        closed(key) => { root.closed-count += 1; root.closed-key = key; }
    }
    tail := Rectangle { y: area.preferred-height; x: 20px; width: 200px; height: 30px; background: #6a5acd; }
}
"#;

fn row(key: &str, title: &str, text: &str, open: bool, rev: i32) -> Value {
    let actions: Vec<Value> = vec![Value::String("Try again".into())];
    Value::Struct(Struct::from_iter([
        ("key".to_string(), Value::String(key.into())),
        ("title".to_string(), Value::String(title.into())),
        ("text".to_string(), Value::String(text.into())),
        ("details".to_string(), Value::String("".into())),
        ("kind".to_string(), Value::String("err".into())),
        ("actions".to_string(), Value::Model(ModelRc::new(VecModel::from(actions)))),
        ("open".to_string(), Value::Bool(open)),
        ("rev".to_string(), Value::Number(f64::from(rev))),
    ]))
}

const SHORT: &str = "Open Telescope on the phone and keep it on screen.";
const LONG: &str = "Open Telescope on the phone and keep the app on screen. The phone needs to be on the same network as this computer, or plugged in with a USB cable, and it has to stay awake the whole time you stream. If the phone sleeps the picture stops until you wake it.";

fn up() -> (slint_interpreter::ComponentInstance, Rc<VecModel<Value>>) {
    let ui = make(DOC, 900.0, 300.0);
    let model = Rc::new(VecModel::<Value>::default());
    set(&ui, "banners", Value::Model(ModelRc::from(model.clone())));
    step(FRAME);
    (ui, model)
}

/// Where the content under the banners is, read as the window is drawn (a banner is made when it is first laid out).
fn tail(ui: &slint_interpreter::ComponentInstance) -> f32 {
    offscreen::draw(ui.window(), 900.0, 300.0, 1.0).unwrap();
    num(ui, "tail-y")
}

fn frames(ui: &slint_interpreter::ComponentInstance, n: usize) -> Vec<f32> {
    let mut out = vec![tail(ui)];
    for _ in 0..n {
        step(FRAME);
        out.push(tail(ui));
    }
    out
}

/// Where the banner called this is: the banner itself and its title are both called by the title, and the banner is the higher.
fn top_of(ui: &slint_interpreter::ComponentInstance, label: &str) -> Option<f32> {
    use i_slint_backend_testing::ElementHandle;
    ElementHandle::find_by_accessible_label(ui, label).map(|b| b.absolute_position().y).min_by(|a, b| a.partial_cmp(b).unwrap())
}

/// The biggest change between two neighbouring frames.
fn biggest_step(v: &[f32]) -> f32 {
    v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

#[test]
fn a_banner_opens_from_nothing_and_what_is_under_it_follows_with_no_jump() {
    let _still = offscreen::freeze_clock();
    let (ui, model) = up();
    assert_eq!(tail(&ui), 0.0, "nothing up, no room");
    model.push(row("start", "Can't connect to the phone", SHORT, true, 0));
    let t = frames(&ui, 60);
    let full = *t.last().unwrap();
    assert!(full > 40.0, "it has its height: {full}");
    assert!(t[1] < full * 0.4, "the first frame is a start, not the whole: {t:?}");
    assert!(t.windows(2).all(|w| w[1] >= w[0] - 0.001), "it only grows, no overshoot to settle back from: {t:?}");
    assert!(t.iter().all(|v| *v <= full + 0.001));
    assert!(biggest_step(&t) < full * 0.25, "no jump: {t:?}");
    assert!(t[20] > full * 0.8 && t[8] < full, "over about a third of a second: {t:?}");
    // At rest it asks for no frames
    let before = tail(&ui);
    step(500);
    assert_eq!(tail(&ui), before);
}

#[test]
fn dismissing_it_mid_way_closes_from_where_it_is_and_only_then_says_closed() {
    let _still = offscreen::freeze_clock();
    let (ui, model) = up();
    let open = |on: bool| row("start", "Can't connect to the phone", SHORT, on, 0);
    model.push(open(true));
    tail(&ui);
    step(1000);
    let full = tail(&ui);
    // Dismissed while it is still opening: it goes back from the place it had reached
    model.set_row_data(0, open(false));
    step(1000);
    assert_eq!(tail(&ui), 0.0);
    model.set_row_data(0, open(true));
    let opening = frames(&ui, 6);
    let reached = *opening.last().unwrap();
    assert!(reached > 0.0 && reached < full, "mid-way: {opening:?}");
    model.set_row_data(0, open(false));
    let closing = frames(&ui, 6);
    let joined: Vec<f32> = opening.iter().chain(closing.iter()).copied().collect();
    assert!((closing[1] - reached).abs() < 6.0 && closing[6] < reached - 4.0, "it turns round from where it is, with no jump: {joined:?}");
    assert!(biggest_step(&joined) < full * 0.25, "{joined:?}");
    // Turned round again while closing
    model.set_row_data(0, open(true));
    let again = frames(&ui, 6);
    let joined: Vec<f32> = closing.iter().chain(again.iter()).copied().collect();
    assert!(biggest_step(&joined) < full * 0.25, "{joined:?}");
    let count = num(&ui, "closed-count") as i32;
    model.set_row_data(0, open(false));
    assert_eq!(num(&ui, "closed-count") as i32, count, "not before it has shut");
    let t = frames(&ui, 60);
    assert_eq!(*t.last().unwrap(), 0.0, "all the way shut: {t:?}");
    assert!(t[4..].windows(2).all(|w| w[1] <= w[0] + 0.001), "once it has turned it only shuts: {t:?}");
    assert_eq!(num(&ui, "closed-count") as i32, count + 1, "and it says so once");
}

#[test]
fn with_two_banners_one_leaving_the_other_slides_to_its_new_place() {
    let _still = offscreen::freeze_clock();
    let (ui, model) = up();
    model.push(row("a", "Can't connect to the phone", SHORT, true, 0));
    model.push(row("b", "The phone stopped streaming", SHORT, true, 0));
    tail(&ui);
    step(1000);
    const B: &str = "The phone stopped streaming";
    let (a_top, b_top_before) = (top_of(&ui, "Can't connect to the phone").unwrap(), top_of(&ui, B).unwrap());
    assert!(b_top_before > a_top + 40.0, "b is under a: {a_top} {b_top_before}");
    let full = tail(&ui);
    model.set_row_data(0, row("a", "Can't connect to the phone", SHORT, false, 0));
    let mut b_top = vec![top_of(&ui, B).unwrap()];
    let mut heights = vec![tail(&ui)];
    for _ in 0..50 {
        step(FRAME);
        b_top.push(top_of(&ui, B).unwrap());
        heights.push(tail(&ui));
    }
    assert!(*b_top.last().unwrap() < b_top_before - 20.0, "b moved up into a's place: {b_top:?}");
    assert!(b_top.windows(2).all(|w| w[1] <= w[0] + 0.001), "{b_top:?}");
    assert!(biggest_step(&b_top) < 12.0, "it slides, no reflow jump: {b_top:?}");
    assert!(biggest_step(&heights) < 12.0, "{heights:?}");
    assert!(*heights.last().unwrap() < full - 20.0 && *heights.last().unwrap() > 20.0, "b is still there: {heights:?}");
    assert_eq!(num(&ui, "closed-count") as i32, 1);
    assert_eq!(ui.get_property("closed-key").unwrap(), Value::String("a".into()));
    // The row goes (as the window does it) and nothing moves
    model.remove(0);
    let after = tail(&ui);
    step(FRAME * 3);
    let settled = tail(&ui);
    assert!((settled - after).abs() < 3.0, "removing the shut row moves nothing much: {after} {settled}");
}

#[test]
fn a_banner_that_says_something_new_cross_fades_and_moves_its_height_and_never_closes() {
    let _still = offscreen::freeze_clock();
    let (ui, model) = up();
    model.push(row("start", "Can't connect to the phone", SHORT, true, 0));
    tail(&ui);
    step(1000);
    let short = tail(&ui);
    model.set_row_data(0, row("start", "Can't connect to the phone", LONG, true, 1));
    let t = frames(&ui, 60);
    let long = *t.last().unwrap();
    assert!(long > short + 8.0, "taller for the longer message: {short} {long}");
    assert!(t.windows(2).all(|w| w[1] >= w[0] - 0.001), "height only grows: {t:?}");
    assert!(biggest_step(&t) < (long - short) * 0.3, "no jump: {t:?}");
    assert_eq!(num(&ui, "closed-count") as i32, 0, "no close and open");
    // Back to the short one, and turned round mid-way
    model.set_row_data(0, row("start", "Can't connect to the phone", SHORT, true, 2));
    let down = frames(&ui, 8);
    model.set_row_data(0, row("start", "Can't connect to the phone", LONG, true, 3));
    let up_again = frames(&ui, 8);
    let joined: Vec<f32> = down.iter().chain(up_again.iter()).copied().collect();
    assert!(biggest_step(&joined) < (long - short) * 0.3, "a change in the middle of a change leaves no seam: {joined:?}");
    step(1000);
    assert!((tail(&ui) - long).abs() < 0.5);
    assert_eq!(num(&ui, "closed-count") as i32, 0);
}

#[test]
fn film_strips() {
    let Some(dir) = std::env::var_os("BANNER_SHOTS").map(std::path::PathBuf::from).or_else(shots) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let _still = offscreen::freeze_clock();
    let (ui, model) = up();
    let crop = (0.0, 0.0, 900.0, 170.0);
    let draw = |name: &str, pics: Vec<offscreen::Picture>| {
        let every: Vec<_> = pics.into_iter().step_by(2).collect();
        save(&dir, &format!("banner-{name}.png"), &strip(&every, crop, 1.0, 2));
    };
    let a = |on: bool, text: &str, rev: i32| row("a", "Can't connect to the phone", text, on, rev);
    let pics = film(&ui, (900.0, 300.0), 1.0, 24, |i| {
        if i == 0 {
            model.push(a(true, SHORT, 0));
        }
    });
    draw("open", pics);
    step(600);
    let pics = film(&ui, (900.0, 300.0), 1.0, 24, |i| {
        if i == 0 {
            model.set_row_data(0, a(false, SHORT, 0));
        }
    });
    draw("close", pics);
    step(600);
    model.set_row_data(0, a(true, SHORT, 0));
    step(600);
    let pics = film(&ui, (900.0, 300.0), 1.0, 28, |i| {
        if i == 0 {
            model.set_row_data(0, a(true, LONG, 1));
        }
    });
    save(&dir, "banner-text-change-early.png", &strip(&pics[..8], crop, 1.0, 2));
    draw("text-change", pics);
    step(600);
    model.push(row("b", "The phone stopped streaming", SHORT, true, 0));
    step(600);
    let pics = film(&ui, (900.0, 300.0), 1.0, 28, |i| {
        if i == 0 {
            model.set_row_data(0, a(false, LONG, 1));
        }
    });
    draw("two-one-leaves", pics);
}
