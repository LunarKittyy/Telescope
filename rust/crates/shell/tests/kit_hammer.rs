// Every motion retargeted over and over, at random moments and back and forth between the same two places, must follow the ideal chain
// of springs (each retarget starting from where the last left the value, with its speed) and never jump, stall or restart. The owner's
// words: the smoothness must hold up infinitely. The reference is the spring Slint solves (natural frequency 2 pi / period, damping
// ratio 1 - bounce) stepped here on its own, with the same retargets at the same frames.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

/// A deterministic run of numbers (no random crate in the tests).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One spring: a value and its speed, given a target, a period and a bounce at each retarget.
#[derive(Clone, Copy)]
struct Spring {
    x: f64,
    v: f64,
    target: f64,
    period: f64,
    bounce: f64,
}

impl Spring {
    fn at(x: f64) -> Self {
        Spring { x, v: 0.0, target: x, period: 0.2, bounce: 0.0 }
    }
    fn retarget(&mut self, target: f64, period: f64, bounce: f64) {
        self.target = target;
        self.period = period;
        self.bounce = bounce;
    }
    fn advance(&mut self, seconds: f64) {
        let w = 2.0 * std::f64::consts::PI / self.period;
        let z = 1.0 - self.bounce;
        let dt = 0.0002;
        let n = (seconds / dt).round() as usize;
        for _ in 0..n {
            let a = -w * w * (self.x - self.target) - 2.0 * z * w * self.v;
            self.v += a * dt;
            self.x += self.v * dt;
        }
    }
}

const DT: f64 = FRAME as f64 / 1000.0;

/// Frames to wait before the next retarget: 20 to 80 ms, sometimes a frame or two (a fast hand).
fn gap(rng: &mut Lcg) -> usize {
    match rng.below(10) {
        0 => 1,
        1 | 2 => 2,
        _ => 2 + rng.below(4) as usize,
    }
}

const MOTIONS: &str = r#"
import { Settle, Flick, Pop, Tone, Fade, Unfold } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 100px;
    height: 100px;
    in property <float> to;
    in property <bool> on;
    out property <float> settle: settle-m.value;
    out property <float> flick: flick-m.value;
    out property <float> pop: pop-m.value;
    out property <float> tone: tone-m.value;
    out property <float> fade: fade-m.value;
    out property <float> unfold: unfold-m.value;
    settle-m := Settle { to: root.to; }
    flick-m := Flick { to: root.to; }
    pop-m := Pop { to: root.to; }
    tone-m := Tone { on: root.on; }
    fade-m := Fade { shown: root.on; }
    unfold-m := Unfold { open: root.on; }
}
"#;

struct Kind {
    name: &'static str,
    // (period, bounce) for a target of 1 and for a target of 0
    up: (f64, f64),
    down: (f64, f64),
    clamped: bool,
    bool_input: bool,
}

const KINDS: [Kind; 6] = [
    Kind { name: "settle", up: (0.2, 0.0), down: (0.2, 0.0), clamped: false, bool_input: false },
    Kind { name: "flick", up: (0.26, 0.22), down: (0.26, 0.22), clamped: false, bool_input: false },
    Kind { name: "pop", up: (0.15, 0.4), down: (0.15, 0.4), clamped: false, bool_input: false },
    Kind { name: "tone", up: (0.09, 0.0), down: (0.14, 0.0), clamped: true, bool_input: true },
    Kind { name: "fade", up: (0.12, 0.0), down: (0.10, 0.0), clamped: true, bool_input: true },
    Kind { name: "unfold", up: (0.17, 0.0), down: (0.13, 0.0), clamped: true, bool_input: true },
];

/// Retarget `kind` 60 times (back and forth, and to random places) and return the worst gap to the ideal chain, in the travel's units.
fn hammer_kind(kind: &Kind, seed: u64, frame_gaps: bool, pre_read: bool) -> (f64, Vec<f32>) {
    let _still = offscreen::freeze_clock();
    let ui = make(MOTIONS, 100.0, 100.0);
    let mut rng = Lcg(seed);
    for k in &KINDS {
        num(&ui, k.name);
    }
    let mut model = Spring::at(0.0);
    let mut worst = 0.0f64;
    let mut seen = Vec::new();
    let mut target = 0.0f64;
    for round in 0..60 {
        // Back and forth between the same two places most of the time, a place in between now and then
        target = if kind.bool_input || round % 5 != 4 { 1.0 - target.round() } else { rng.below(100) as f64 / 100.0 };
        // The value is read at the moment of the retarget (as the control's own callback does): Slint starts the new spring from the
        // value the property last evaluated, so one that wasn't read this frame is a frame behind and the move stalls
        if pre_read {
            num(&ui, kind.name);
        }
        if kind.bool_input {
            set(&ui, "on", Value::Bool(target == 1.0));
        } else {
            set(&ui, "to", Value::Number(target));
        }
        let (period, bounce) = if target >= model.x { kind.up } else { kind.down };
        model.retarget(target, period, bounce);
        let frames = if frame_gaps { gap(&mut rng) } else { 3 };
        for _ in 0..frames {
            let got = num(&ui, kind.name) as f64;
            let want = if kind.clamped { model.x.clamp(0.0, 1.0) } else { model.x };
            worst = worst.max((got - want).abs());
            seen.push(got as f32);
            step(FRAME);
            model.advance(DT);
        }
    }
    (worst, seen)
}

#[test]
fn every_motion_follows_its_ideal_chain_of_springs_under_sixty_retargets() {
    for kind in &KINDS {
        for seed in [1u64, 7, 99] {
            let (worst, seen) = hammer_kind(kind, seed, true, true);
            assert!(worst < 0.06, "{} (seed {seed}) left the ideal chain by {worst}: {seen:?}", kind.name);
        }
    }
}

#[test]
fn no_motion_jumps_or_stalls_between_two_frames_however_it_is_retargeted() {
    // The step between two frames can't be bigger than the fastest a spring of that period could cover, whatever the retargets
    for kind in &KINDS {
        for seed in [3u64, 11] {
            let (_, seen) = hammer_kind(kind, seed, true, true);
            let fastest = 0.45f32; // a retarget bends a move, it never makes one faster than a fraction of the travel a frame
            for w in seen.windows(2) {
                assert!((w[1] - w[0]).abs() <= fastest, "{} jumped {} -> {}: {seen:?}", kind.name, w[0], w[1]);
            }
        }
    }
}

const ROW: &str = r#"
import { Segmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 40px;
    background: Theme.card;
    in-out property <int> cur: 0;
    out property <length> l: seg.worm-left;
    out property <length> r: seg.worm-right;
    seg := Segmented {
        x: 10px; y: 10px; width: 280px;
        label: "Pick";
        items: ["One", "Two", "Three", "Four"];
        disabled: [false, false, false, false];
        current: root.cur;
        hue: Theme.accent;
    }
}
"#;

const GRID: &str = r#"
import { PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 120px;
    background: Theme.card;
    in-out property <int> cur: 0;
    out property <length> l: pics.worm-left;
    out property <length> r: pics.worm-right;
    out property <length> t: pics.worm-top;
    out property <length> b: pics.worm-bottom;
    out property <length> pitch: pics.pitch;
    out property <length> row-pitch: pics.row-pitch;
    out property <length> cell: pics.cell-width;
    pics := PictureSegmented {
        x: 10px; y: 8px; width: 280px; label: "Style"; items: ["Bar", "Pill", "Light", "Shadow"]; disabled: [false, false, false, false];
        current: root.cur; columns: 2; hue: Theme.accent;
        Rectangle { }
    }
}
"#;

/// An edge of the worm in the model: which spring it is on depends on which way the pick went.
fn edge_period(lead: bool, pace: f64) -> (f64, f64) {
    if lead { (0.110 * pace, 0.0) } else { (0.280 * pace, 0.3) }
}

#[test]
fn the_worm_in_a_row_follows_its_ideal_chain_under_many_picks_and_never_snaps() {
    for seed in [5u64, 21, 77] {
        let _still = offscreen::freeze_clock();
        let ui = make(ROW, 300.0, 40.0);
        step(FRAME);
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 40.0, 1.0).unwrap();
        num(&ui, "l");
        let gapw = 6.0;
        let w = (280.0 - 3.0 * gapw) / 4.0;
        let cell = |i: usize| (i as f64 * (w + gapw), i as f64 * (w + gapw) + w);
        let mut rng = Lcg(seed);
        let (mut left, mut right) = (Spring::at(cell(0).0), Spring::at(cell(0).1));
        let mut cx = (cell(0).0 + cell(0).1) / 2.0;
        let mut cur = 0usize;
        let mut worst = 0.0f64;
        let mut trace = Vec::new();
        for round in 0..60 {
            // Back and forth between two neighbours most of the time, and a far one now and then
            let next = if round % 4 == 3 { rng.below(4) as usize } else if cur == 1 { 2 } else if cur == 2 { 1 } else { 1 + (rng.below(2) as usize) };
            if next != cur {
                cur = next;
                // Clicked, as a person does (the control reads its springs first, so a retarget loses nothing)
                let (nl, nr) = cell(cur);
                press(&ui, 10.0 + (nl + nr) as f32 / 2.0, 26.0);
                release(&ui, 10.0 + (nl + nr) as f32 / 2.0, 26.0);
                let ncx = (nl + nr) / 2.0;
                let fwd = ncx > cx;
                cx = ncx;
                let (p, b) = edge_period(!fwd, 1.0);
                left.retarget(nl, p, b);
                let (p, b) = edge_period(fwd, 1.0);
                right.retarget(nr, p, b);
            }
            for _ in 0..gap(&mut rng) {
                let (l, r) = (num(&ui, "l") as f64, num(&ui, "r") as f64);
                worst = worst.max((l - left.x).abs()).max((r - right.x).abs());
                trace.push((l as f32, r as f32));
                step(FRAME);
                left.advance(DT);
                right.advance(DT);
            }
        }
        assert!(worst < 7.0, "seed {seed}: the worm left the ideal chain by {worst}px: {trace:?}");
        // And it lands where it was last sent
        for _ in 0..60 {
            step(FRAME);
        }
        let (nl, nr) = cell(cur);
        assert!((num(&ui, "l") as f64 - nl).abs() < 0.2 && (num(&ui, "r") as f64 - nr).abs() < 0.2, "ends on option {cur}");
    }
}

#[test]
fn the_worm_in_a_grid_goes_straight_between_cells_and_follows_its_chain_under_many_picks() {
    for seed in [2u64, 13, 60] {
        let _still = offscreen::freeze_clock();
        let ui = make(GRID, 300.0, 120.0);
        step(FRAME);
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
        num(&ui, "l");
        let (pitch, row_pitch, cellw) = (num(&ui, "pitch") as f64, num(&ui, "row-pitch") as f64, num(&ui, "cell") as f64);
        let tile_h = row_pitch - 6.0;
        let cell = |i: usize| ((i % 2) as f64 * pitch, (i / 2) as f64 * row_pitch);
        let mut rng = Lcg(seed);
        let (c0x, c0y) = cell(0);
        let (mut l, mut r, mut t, mut b) = (Spring::at(c0x), Spring::at(c0x + cellw), Spring::at(c0y), Spring::at(c0y + tile_h));
        let (mut cx, mut cy) = (c0x + cellw / 2.0, c0y + tile_h / 2.0);
        let (mut fwd, mut down) = (true, true);
        let mut cur = 0usize;
        let mut worst = 0.0f64;
        let mut trace = Vec::new();
        for round in 0..80 {
            // Light <-> Pill (the diagonal), Bar <-> Shadow, and any
            let next = match round % 6 { 0 | 1 => if cur == 2 { 1 } else { 2 }, 2 | 3 => if cur == 0 { 3 } else { 0 }, _ => rng.below(4) as usize };
            if next != cur {
                cur = next;
                let (nx, ny) = cell(cur);
                press(&ui, 10.0 + (nx + cellw / 2.0) as f32, 8.0 + (ny + tile_h / 2.0) as f32);
                release(&ui, 10.0 + (nx + cellw / 2.0) as f32, 8.0 + (ny + tile_h / 2.0) as f32);
                let (ncx, ncy) = (nx + cellw / 2.0, ny + tile_h / 2.0);
                let h_moved = ncx != cx;
                let v_moved = ncy != cy;
                // Which way it went is kept for the axis that did not move, as the worm keeps it
                if h_moved { fwd = ncx > cx; }
                if v_moved { down = ncy > cy; }
                let (hp, vp) = if h_moved && v_moved { (0.9, 1.3) } else { (1.0, 1.0) };
                // A spring that is given the same target is not restarted, so its period stays what it was
                if h_moved {
                    let p = edge_period(!fwd, hp);
                    l.retarget(nx, p.0, p.1);
                    let p = edge_period(fwd, hp);
                    r.retarget(nx + cellw, p.0, p.1);
                }
                if v_moved {
                    let p = edge_period(!down, vp);
                    t.retarget(ny, p.0, p.1);
                    let p = edge_period(down, vp);
                    b.retarget(ny + tile_h, p.0, p.1);
                }
                cx = ncx;
                cy = ncy;
            }
            for _ in 0..gap(&mut rng) {
                let got = [num(&ui, "l"), num(&ui, "r"), num(&ui, "t"), num(&ui, "b")];
                let want = [l.x, r.x, t.x, b.x];
                for i in 0..4 {
                    worst = worst.max((got[i] as f64 - want[i]).abs());
                }
                trace.push(got);
                step(FRAME);
                for s in [&mut l, &mut r, &mut t, &mut b] {
                    s.advance(DT);
                }
            }
        }
        assert!(worst < 7.0, "seed {seed}: the grid worm left the ideal chain by {worst}px: {trace:?}");
        for _ in 0..60 {
            step(FRAME);
        }
        let (nx, ny) = cell(cur);
        let got = (num(&ui, "l") as f64, num(&ui, "t") as f64);
        assert!((got.0 - nx).abs() < 0.2 && (got.1 - ny).abs() < 0.2, "ends on cell {cur}: {got:?}");
    }
}

const SWITCH: &str = r#"
import { Switch, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 120px;
    height: 36px;
    background: Theme.card;
    in-out property <bool> on;
    out property <float> place: sw.handle-place;
    sw := Switch { x: 40px; y: 6px; checked <=> root.on; label: "Flip"; hue: Theme.accent; }
}
"#;

#[test]
fn a_switch_clicked_sixty_times_at_random_moments_follows_its_chain() {
    for seed in [4u64, 40] {
        let _still = offscreen::freeze_clock();
        let ui = make(SWITCH, 120.0, 36.0);
        num(&ui, "place");
        let mut rng = Lcg(seed);
        let mut model = Spring::at(0.0);
        let mut target = 0.0;
        let mut worst = 0.0f64;
        let mut trace = Vec::new();
        for _ in 0..60 {
            press(&ui, 50.0, 18.0);
            release(&ui, 50.0, 18.0);
            target = 1.0 - target;
            model.retarget(target, 0.26, 0.22);
            for _ in 0..gap(&mut rng) {
                let got = num(&ui, "place") as f64;
                worst = worst.max((got - model.x).abs());
                trace.push(got as f32);
                step(FRAME);
                model.advance(DT);
            }
        }
        assert!(worst < 0.06, "seed {seed}: the handle left the ideal chain by {worst}: {trace:?}");
    }
}
