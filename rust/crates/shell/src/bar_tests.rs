use super::*;
use telescope_core::bar::{BarStat, Side, CAMERA, FPS, THROUGHPUT};

fn stat(id: &str, value: &str) -> BarStat {
    BarStat::new(id, id, "", value)
}

/// A bar 600 wide with the three readouts reading, each 40 wide.
fn bar(width: f32) -> Bar {
    let mut b = Bar::new();
    b.set_metrics(Metrics { width, pad: 16.0, gap: 16.0, chip: 24.0 });
    b.set_own(vec![stat(CAMERA, "Phone"), stat(FPS, "30"), stat(THROUGHPUT, "12 Mbps")]);
    b
}

fn row<'a>(v: &'a View, id: &str) -> &'a PinRow {
    v.rows.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("no row {id}"))
}

#[test]
fn the_defaults_are_the_camera_on_the_left_and_the_frame_rate_and_throughput_on_the_right() {
    let mut b = bar(600.0);
    assert_eq!(b.persisted(), (vec!["camera".to_string()], vec!["fps".to_string(), "throughput".to_string()]));
    let v = b.view();
    assert_eq!(row(&v, CAMERA).x, 16.0);
    assert_eq!(row(&v, CAMERA).side, Side::Left);
    // The right side stacks from the right edge: throughput last, the frame rate before it
    assert_eq!(row(&v, THROUGHPUT).x, 600.0 - 16.0 - 40.0);
    assert_eq!(row(&v, FPS).x, 600.0 - 16.0 - 40.0 - 16.0 - 40.0);
    assert!(v.rows.iter().all(|r| r.state == State::Shown) && v.left.is_none() && v.right.is_none());
}

#[test]
fn each_side_stacks_from_its_own_edge() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, "Phone"), stat(FPS, "30"), stat(THROUGHPUT, "12"), stat("stats.cpu", "5%")]);
    assert!(b.pin("stats.cpu", true));
    let v = b.view();
    // A new pin goes to the inner end of the right side
    assert_eq!(b.persisted().1, ["stats.cpu", "fps", "throughput"]);
    assert!(row(&v, "stats.cpu").x < row(&v, FPS).x);
    // On the left, a second pin sits after the first
    b.drag_start("stats.cpu");
    b.drag_to("stats.cpu", 100.0);
    assert!(b.drag_end("stats.cpu"));
    let v = b.view();
    assert_eq!(b.persisted().0, ["camera", "stats.cpu"]);
    assert_eq!(row(&v, "stats.cpu").x, 16.0 + 40.0 + 16.0);
    assert_eq!(row(&v, "stats.cpu").side, Side::Left);
}

#[test]
fn a_pin_with_nothing_to_read_takes_no_room_and_keeps_its_place() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, ""), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    let v = b.view();
    assert_eq!(row(&v, CAMERA).state, State::Hidden);
    assert_eq!(b.persisted().0, [CAMERA]);
    // The pin of a plugin that is not there is just as quiet
    b.pin("gone.thing", true);
    let v = b.view();
    assert_eq!(row(&v, "gone.thing").state, State::Hidden);
    assert_eq!(row(&v, THROUGHPUT).x, 600.0 - 16.0 - 40.0, "nothing is left for it");
}

fn pins(widths: &[f32]) -> Vec<(f32, bool)> {
    widths.iter().map(|w| (*w, false)).collect()
}

fn count(flags: &[bool]) -> usize {
    flags.iter().filter(|f| **f).count()
}

#[test]
fn nothing_folds_while_it_fits() {
    let fit = fold(&pins(&[40.0, 40.0]), &pins(&[40.0, 40.0]), 230.0, 16.0, 24.0);
    assert_eq!((count(&fit.left), count(&fit.right)), (0, 0));
}

#[test]
fn the_fuller_side_folds_its_innermost_pins_first() {
    // Left 3 pins (40 each, 112 + gaps = 152), right 1 pin (40): room 200 needs the left to give one up
    let fit = fold(&pins(&[40.0, 40.0, 40.0]), &pins(&[40.0]), 200.0, 16.0, 24.0);
    assert_eq!((fit.left.clone(), fit.right.clone()), (vec![false, false, true], vec![false]));
    // Both sides full: the wider one goes first, and they take turns as the other becomes the wider
    let fit = fold(&pins(&[40.0, 40.0, 40.0]), &pins(&[40.0, 40.0, 40.0]), 200.0, 16.0, 24.0);
    assert!(count(&fit.left) > 0 && count(&fit.right) > 0, "{fit:?}");
    assert!((count(&fit.left) as i32 - count(&fit.right) as i32).abs() <= 1, "{fit:?}");
    // Folding down to a chip alone
    let fit = fold(&pins(&[40.0, 40.0]), &[], 30.0, 16.0, 24.0);
    assert_eq!((count(&fit.left), count(&fit.right)), (2, 0));
}

#[test]
fn a_sticky_pin_is_never_folded_whatever_the_room() {
    // The camera is the outermost on the left; everything else folds, it stays however narrow
    let left = vec![(120.0, true), (40.0, false), (40.0, false)];
    let fit = fold(&left, &pins(&[40.0, 40.0]), 100.0, 16.0, 24.0);
    assert_eq!(fit.left, vec![false, true, true]);
    assert_eq!(fit.right, vec![true, true]);
    // A sticky pin on the inner end doesn't stop the ones outside it from folding, innermost foldable first
    let left = vec![(40.0, false), (40.0, false), (120.0, true)];
    let fit = fold(&left, &[], 150.0, 16.0, 24.0);
    assert_eq!(fit.left, vec![true, true, false]);
}

#[test]
fn a_narrow_bar_folds_the_innermost_pins_and_the_chip_sits_at_the_inner_end() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, "Phone"), stat("a", "1"), stat("b", "1"), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    b.load(Some((vec!["camera".into(), "a".into(), "b".into()], vec!["fps".into(), "throughput".into()])));
    b.set_metrics(Metrics { width: 232.0, pad: 16.0, gap: 16.0, chip: 24.0 });
    let v = b.view();
    // The camera, the outermost left pin, stays; the innermost left pins fold
    assert_eq!(row(&v, CAMERA).state, State::Shown);
    assert_eq!(row(&v, "b").state, State::Folded);
    assert_eq!(row(&v, THROUGHPUT).state, State::Shown, "the outermost pin of each side is the last to fold");
    let chip = v.left.expect("a chip on the fuller side");
    assert!(chip.count >= 1);
    // Folded pins sit in the chip
    assert_eq!(row(&v, "b").x, chip.x);
    // No pin changed side
    assert_eq!(b.persisted().0, ["camera", "a", "b"]);
}

#[test]
fn one_full_side_folds_alone() {
    let mut b = bar(600.0);
    let ids: Vec<String> = (0..6).map(|i| format!("p{i}")).collect();
    b.set_own(ids.iter().map(|i| stat(i, "1")).collect());
    b.load(Some((vec![], ids.clone())));
    b.set_metrics(Metrics { width: 232.0, pad: 16.0, gap: 16.0, chip: 24.0 });
    let v = b.view();
    assert!(v.left.is_none());
    let chip = v.right.expect("the right side folds");
    // The right list is drawn left to right, so its first pins are the innermost and fold first
    assert_eq!(row(&v, "p0").state, State::Folded);
    assert_eq!(row(&v, "p5").state, State::Shown);
    assert_eq!(chip.count, v.rows.iter().filter(|r| r.state == State::Folded).count());
    let shown_left_edge = v.rows.iter().filter(|r| r.state == State::Shown).map(|r| r.x).fold(f32::MAX, f32::min);
    assert!(chip.x + 24.0 + 16.0 <= shown_left_edge + 0.01, "the chip is inside the shown pins");
}

#[test]
fn the_pins_come_back_when_the_bar_widens() {
    let mut b = bar(600.0);
    b.set_metrics(Metrics { width: 100.0, pad: 16.0, gap: 16.0, chip: 24.0 });
    assert!(b.view().rows.iter().any(|r| r.state == State::Folded));
    b.set_metrics(Metrics { width: 600.0, pad: 16.0, gap: 16.0, chip: 24.0 });
    assert!(b.view().rows.iter().all(|r| r.state == State::Shown));
}

#[test]
fn a_pin_moves_to_a_slot_and_the_others_make_room() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, "Phone"), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    // Drag the frame rate to the right of the throughput
    b.drag_start(FPS);
    b.drag_to(FPS, 590.0);
    let v = b.view();
    assert_eq!(b.dragging(), Some(FPS));
    assert!(row(&v, THROUGHPUT).x < row(&v, FPS).x, "while it is held its slot is at the edge and the throughput made room");
    assert!(b.drag_end(FPS));
    assert_eq!(b.persisted().1, ["throughput", "fps"]);
    // Across to the left side, in front of the camera
    b.drag_start(FPS);
    b.drag_to(FPS, 5.0);
    assert!(b.drag_end(FPS));
    assert_eq!(b.persisted(), (vec!["fps".to_string(), "camera".to_string()], vec!["throughput".to_string()]));
    // Let go where it was: nothing changed
    b.drag_start(CAMERA);
    b.drag_to(CAMERA, 120.0);
    assert!(!b.drag_end(CAMERA) || b.persisted().0 == ["fps", "camera"]);
}

#[test]
fn a_pin_never_changes_side_by_itself() {
    let mut b = bar(600.0);
    let before = b.persisted();
    for width in [900.0, 120.0, 60.0, 600.0] {
        b.set_metrics(Metrics { width, pad: 16.0, gap: 16.0, chip: 24.0 });
        b.view();
    }
    assert_eq!(b.persisted(), before);
}

#[test]
fn the_camera_cannot_be_unpinned_while_two_streams_run() {
    let mut b = bar(600.0);
    assert!(b.pin(CAMERA, false), "one stream: it can go");
    assert!(!b.is_pinned(CAMERA));
    b.set_streams(2);
    assert!(b.is_pinned(CAMERA), "two streams bring it back, in the view only");
    assert!(b.pinned().contains(&CAMERA.to_string()));
    let v = b.view();
    assert_eq!(row(&v, CAMERA).state, State::Shown);
    assert!(row(&v, CAMERA).locked);
    assert!(!b.pin(CAMERA, false), "refused");
    b.set_streams(1);
    assert!(!b.is_pinned(CAMERA), "and it goes again when one stream is left");
    assert!(b.pin(CAMERA, true));
    b.set_streams(3);
    assert!(!b.pin(CAMERA, false));
    assert!(b.is_pinned(CAMERA));
}

#[test]
fn a_plugins_offers_replace_the_last_and_go_with_the_plugin() {
    let mut b = bar(600.0);
    b.offer("stats", &[stat("stats.cpu", "5%")]);
    assert_eq!(b.stat("stats.cpu").map(|s| s.value.as_str()), Some("5%"));
    b.offer("stats", &[stat("stats.cpu", "9%")]);
    assert_eq!(b.stat("stats.cpu").map(|s| s.value.as_str()), Some("9%"));
    b.offer("stats", &[]);
    assert!(b.stat("stats.cpu").is_none());
    // Its pin is kept, quiet, for when the plugin returns
    b.pin("stats.cpu", true);
    assert!(b.is_pinned("stats.cpu"));
    assert_eq!(row(&b.view(), "stats.cpu").state, State::Hidden);
}

#[test]
fn a_row_keeps_its_pin_and_a_returning_pin_is_told() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, ""), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    let first: Vec<String> = b.view().rows.iter().map(|r| r.id.clone()).collect();
    b.set_own(vec![stat(CAMERA, "Phone"), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    let v = b.view();
    assert_eq!(v.returning, [CAMERA]);
    assert_eq!(v.rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), first, "the rows never reorder");
    b.pin("stats.cpu", true);
    b.offer("stats", &[stat("stats.cpu", "5%")]);
    let ids: Vec<String> = b.view().rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(&ids[..first.len()], &first[..], "a new pin gets a row at the end");
}

#[test]
fn widths_hold_the_widest_value_seen_and_are_forgotten_when_nothing_shows() {
    let mut b = bar(600.0);
    b.measured(FPS, 30.0, 30.0);
    b.measured(FPS, 44.0, 44.0);
    b.measured(FPS, 36.0, 36.0);
    assert_eq!(row(&b.view(), FPS).w, 44.0);
    b.set_own(vec![stat(CAMERA, ""), stat(FPS, ""), stat(THROUGHPUT, "")]);
    b.view();
    b.set_own(vec![stat(CAMERA, "Phone"), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    assert_eq!(row(&b.view(), FPS).w, 40.0, "measured again for the next stream");
}

#[test]
fn a_long_name_is_cut_to_a_share_of_the_room() {
    let mut b = bar(600.0);
    b.measured(CAMERA, 900.0, 900.0);
    assert!(row(&b.view(), CAMERA).w <= (600.0 - 32.0) * 0.45 + 0.01);
}

#[test]
fn the_camera_is_the_last_to_fold_and_the_left_chip_sits_after_it() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, "Phone"), stat("a", "1"), stat("b", "1"), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    b.load(Some((vec!["camera".into(), "a".into(), "b".into()], vec!["fps".into(), "throughput".into()])));
    b.measured(CAMERA, 171.0, 88.0);
    for width in [220.0, 160.0, 120.0] {
        b.set_metrics(Metrics { width, pad: 16.0, gap: 16.0, chip: 24.0 });
        let v = b.view();
        assert_eq!(row(&v, CAMERA).state, State::Shown, "the camera stays at {width}");
        assert_eq!(row(&v, "a").state, State::Folded, "{width}");
        let chip = v.left.expect("the left side's chip");
        assert_eq!(chip.x, 16.0 + row(&v, CAMERA).w + 16.0, "the chip is at the inner end of the left side, after the camera: {width}");
        assert_eq!(chip.count, 2);
    }
}

#[test]
fn a_pin_too_wide_for_its_share_takes_only_what_it_needs_without_its_dim_text() {
    let mut b = bar(600.0);
    b.measured(CAMERA, 171.0, 88.0);
    // Room for all of it: the whole width
    assert_eq!(row(&b.view(), CAMERA).w, 171.0);
    // Cut to a share of a small bar, the dim text goes and so does its room
    b.set_metrics(Metrics { width: 330.0, pad: 16.0, gap: 16.0, chip: 24.0 });
    assert_eq!(row(&b.view(), CAMERA).w, 88.0);
}

#[test]
fn a_drop_with_the_camera_forced_in_lands_where_it_was_shown() {
    let mut b = bar(900.0);
    b.set_own(vec![stat(CAMERA, "Phone"), stat("a", "1"), stat("b", "1"), stat(FPS, "30")]);
    b.load(Some((vec!["a".into(), "b".into()], vec!["fps".into()])));
    b.set_streams(2);
    // Shown as [camera, a, b]; drop the frame rate between the camera and a
    b.drag_start(FPS);
    b.drag_to(FPS, 60.0);
    b.view();
    b.drag_end(FPS);
    assert_eq!(b.persisted().0, vec!["fps".to_string(), "a".to_string(), "b".to_string()], "the camera was never pinned: it is not saved, and does not move the drop a slot");
    // The camera stays in front of it on screen
    let v = b.view();
    assert!(row(&v, CAMERA).x < row(&v, FPS).x && row(&v, FPS).x < row(&v, "a").x, "{:?}", v.rows.iter().map(|r| (&r.id, r.x)).collect::<Vec<_>>());
}

#[test]
fn the_status_word_makes_room_for_the_pins_on_the_left() {
    let mut b = bar(600.0);
    b.set_own(vec![stat(CAMERA, ""), stat(FPS, "30"), stat(THROUGHPUT, "12")]);
    b.load(Some((vec!["fps".into()], vec!["throughput".into()])));
    let v = b.view();
    let fps = row(&v, FPS);
    assert_eq!(v.status_x, fps.x + fps.w + 16.0);
    assert!(v.status_x + v.status_room <= row(&v, THROUGHPUT).x, "and ends before the right side");
    // With nothing on the left it starts at the edge
    b.load(Some((vec![], vec!["fps".into(), "throughput".into()])));
    assert_eq!(b.view().status_x, 16.0);
}
