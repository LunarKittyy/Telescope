use super::*;

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn every_easing_starts_at_nothing_and_lands_exactly() {
    for ease in Ease::ALL {
        assert!(near(spatial(ease, 0.0), 0.0) && near(spatial(ease, 1.0), 1.0), "{ease:?}");
        assert!(near(effect(ease, 0.0), 0.0) && near(effect(ease, 1.0), 1.0), "{ease:?}");
    }
}

#[test]
fn springs_overshoot_by_their_bounce_and_smooth_and_even_never_do() {
    let peak = |ease| (0..=1000).map(|i| spatial(ease, i as f64 / 1000.0)).fold(0.0f64, f64::max);
    assert!((1.005..1.06).contains(&peak(Ease::Springy)), "{}", peak(Ease::Springy));
    assert!(peak(Ease::Bouncy) > 1.1, "{}", peak(Ease::Bouncy));
    assert!(peak(Ease::Smooth) <= 1.0 + 1e-9 && peak(Ease::Even) <= 1.0 + 1e-9);
    // an effect is held inside 0 and 1 whatever the easing
    assert!((0..=1000).all(|i| (0.0..=1.0).contains(&effect(Ease::Bouncy, i as f64 / 1000.0))));
}

#[test]
fn smooth_only_ever_goes_forward() {
    let mut last = 0.0;
    for i in 0..=1000 {
        let v = spatial(Ease::Smooth, i as f64 / 1000.0);
        assert!(v + 1e-9 >= last, "{i}: {v} after {last}");
        last = v;
    }
}

#[test]
fn a_spec_is_cleaned_and_reduced_motion_fades_and_off_cuts() {
    assert_eq!(Spec::clean(None), Spec::default());
    let s = Spec::clean(Some(&json!({"kind": "push", "ms": 99999, "ease": "bouncy"})));
    assert_eq!((s.kind, s.ms, s.ease), (Kind::Push, MAX_MS, Ease::Bouncy));
    assert_eq!(Spec::clean(Some(&json!({"kind": "nonsense", "ms": "x", "ease": 3}))), Spec::default());
    assert_eq!(Spec::clean(Some(&s.dict())), s);
    let reduced = s.for_motion(true, false);
    assert_eq!((reduced.kind, reduced.ms), (Kind::Crossfade, REDUCED_MS));
    assert_eq!(s.for_motion(true, true).kind, Kind::Cut);
    assert_eq!(s.for_motion(false, false), s);
}

fn sample() -> (Vec<Tile>, Vec<Tile>) {
    let from = vec![Tile::boxed("a", "", 0.0, 0.0, 1.0, 1.0), Tile::boxed("b", "", 0.7, 0.7, 0.25, 0.25), Tile::boxed("gone", "", 0.0, 0.7, 0.2, 0.2)];
    let to = vec![Tile::boxed("b", "", 0.0, 0.0, 0.5, 0.5), Tile::boxed("a", "", 0.5, 0.0, 0.5, 0.5), Tile::boxed("new", "", 0.5, 0.5, 0.3, 0.3)];
    (from, to)
}

fn find<'a>(posed: &'a [Posed], id: &str) -> &'a Posed {
    posed.iter().find(|p| p.tile.source_id == id).unwrap()
}

#[test]
fn a_glide_starts_on_the_old_layout_and_ends_on_the_new_one() {
    let (from, to) = sample();
    for kind in [Kind::Glide, Kind::Shuffle] {
        let start = poses(kind, &from, &to, 0.0, Ease::Springy, 16.0 / 9.0);
        let a = find(&start, "a");
        assert!(near(a.tile.w, 1.0) && near(a.tile.x, 0.0) && a.alpha == 1.0, "{kind:?} {a:?}");
        assert!(near(find(&start, "gone").alpha, 1.0) && near(find(&start, "new").alpha, 0.0));
        let end = poses(kind, &from, &to, 1.0, Ease::Springy, 16.0 / 9.0);
        assert!(end.iter().all(|p| p.tile.source_id != "gone" || p.alpha == 0.0));
        for want in &to {
            let got = find(&end, &want.source_id);
            assert!(near(got.tile.x, want.x) && near(got.tile.y, want.y) && near(got.tile.w, want.w) && near(got.tile.h, want.h), "{kind:?} {}: {:?}", want.source_id, got.tile);
            assert!(near(got.alpha, 1.0));
        }
    }
}

#[test]
fn a_tile_stretches_towards_where_it_goes_the_leading_edge_first() {
    let (from, to) = sample();
    // "a" shrinks into the top right: its right edge stays, the left edge trails behind the stiffer spring
    let early = poses(Kind::Glide, &from, &to, 0.2, Ease::Smooth, 16.0 / 9.0);
    let a = find(&early, "a");
    // the leading edge (the bottom here, as it moves down and to the left a little less than... both) is nearer its goal than the trailing one
    let lead_done = (1.0 - (a.tile.y + a.tile.h)) / 0.5;
    let trail_done = a.tile.x / 0.5;
    assert!(lead_done > 0.0 && trail_done >= 0.0 && lead_done != trail_done, "{lead_done} {trail_done}");
}

#[test]
fn shuffle_sets_tiles_off_one_after_another_and_lifts_them_on_the_way() {
    let (from, to) = sample();
    let glide = poses(Kind::Glide, &from, &to, 0.3, Ease::Smooth, 16.0 / 9.0);
    let shuffle = poses(Kind::Shuffle, &from, &to, 0.3, Ease::Smooth, 16.0 / 9.0);
    // the second tile has barely left in a shuffle; in a glide it is well on its way
    let moved = |p: &[Posed]| (find(p, "a").tile.x - 0.0).abs();
    assert!(moved(&shuffle) < moved(&glide), "{} {}", moved(&shuffle), moved(&glide));
    // the first one is lifted by its flight
    let (g, s) = (find(&glide, "b"), find(&shuffle, "b"));
    assert!(s.tile.y < g.tile.y - 0.001, "{} {}", s.tile.y, g.tile.y);
}

#[test]
fn whole_picture_transitions_begin_on_the_old_picture_and_end_on_the_new_one() {
    for kind in [Kind::Crossfade, Kind::Slide, Kind::Push, Kind::Zoom, Kind::Wipe, Kind::Blinds, Kind::Curtain, Kind::Pixelate] {
        for s in [0.1, 0.5, 0.9] {
            for vertical in [false, true] {
                let (first, last) = (cell(kind, vertical, s, 0.0), cell(kind, vertical, s, 1.0));
                // at the start every pixel's mix of B is 0 along one axis at least, and A is where it was
                assert!(near(first.a, s), "{kind:?} {vertical} at the start: {first:?}");
                assert!(near(last.b, s) && near(last.mix, 1.0), "{kind:?} {vertical} at the end: {last:?}");
            }
            assert!(near(cell(kind, false, s, 0.0).mix * cell(kind, true, s, 0.0).mix, 0.0), "{kind:?} shows nothing of the new picture at the start");
        }
    }
}

#[test]
fn the_iris_and_the_dissolve_start_with_none_of_the_new_picture_and_end_with_all_of_it() {
    for kind in [Kind::Iris, Kind::Dissolve] {
        for (x, y) in [(0.0, 0.0), (959.0, 539.0), (1919.0, 1079.0), (300.0, 800.0)] {
            assert!(near(mix_at(kind, x, y, (1920.0, 1080.0), 0.0), 0.0), "{kind:?} at ({x}, {y}) at the start");
            assert!(near(mix_at(kind, x, y, (1920.0, 1080.0), 1.0), 1.0), "{kind:?} at ({x}, {y}) at the end");
        }
    }
    // the iris opens from the middle
    assert!(mix_at(Kind::Iris, 960.0, 540.0, (1920.0, 1080.0), 0.3) > mix_at(Kind::Iris, 10.0, 10.0, (1920.0, 1080.0), 0.3));
}

#[test]
fn a_bounce_lands_with_hops_that_die_away() {
    let from = vec![Tile::boxed("a", "", 0.0, 0.2, 0.4, 0.4)];
    let to = vec![Tile::boxed("a", "", 0.6, 0.2, 0.4, 0.4)];
    let y = |t: f64| poses(Kind::Bounce, &from, &to, t, Ease::Smooth, 16.0 / 9.0)[0].tile.y;
    assert!(near(y(0.0), 0.2) && near(y(1.0), 0.2), "it starts and ends on the line");
    assert!(y(0.4) < 0.2 - 0.005, "it hops up on the way: {}", y(0.4));
    // later hops are smaller than the first
    let up = |lo: f64, hi: f64| (0..=50).map(|i| 0.2 - y(lo + (hi - lo) * i as f64 / 50.0)).fold(0.0f64, f64::max);
    assert!(up(0.0, 0.34) > up(0.67, 1.0));
}
