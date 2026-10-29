use super::*;

const W: usize = 320;
const H: usize = 180;
const CORNERS: [P; 4] = [[110.0, 20.0], [210.0, 28.0], [222.0, 170.0], [98.0, 162.0]];
const A4: f64 = 210.0 / 297.0;

/// A dark desk with a white sheet at CORNERS and a black bar across the sheet's middle, as RGB.
fn desk_frame() -> Vec<u8> {
    let matrix = perspective(&[[0.0, 0.0], [150.0, 0.0], [150.0, 200.0], [0.0, 200.0]], &CORNERS);
    let inv = invert(&matrix);
    let mut frame = vec![40u8; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f64, y as f64);
            let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
            let (sx, sy) = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) / d, (inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) / d);
            if (0.0..150.0).contains(&sx) && (0.0..200.0).contains(&sy) {
                let bar = (95.0..105.0).contains(&sy) && (20.0..130.0).contains(&sx);
                frame[(y * W + x) * 3..][..3].fill(if bar { 0 } else { 240 });
            }
        }
    }
    frame
}

fn full(points: &[P; 4]) -> [P; 4] {
    points.map(|[x, y]| [x / W as f64, y / H as f64])
}

fn sheet(corners: [P; 4], shape: &str) -> Sheet {
    Sheet { corners, cleanup: "off".into(), shape: shape.into(), stretch: [100, 100], geometry: Geometry::default() }
}

fn straight(frame: &[u8], s: &Sheet) -> Vec<u8> {
    straighten(frame, W, H, s, &warp_for(s, W, H))
}

fn at(out: &[u8], w: usize, x: usize, y: usize) -> u8 {
    out[(y * w + x) * 3]
}

#[test]
fn corners_come_out_clockwise_from_the_top_left_however_they_were_placed() {
    assert_eq!(order_corners(&[CORNERS[2], CORNERS[0], CORNERS[3], CORNERS[1]]), CORNERS);
}

#[test]
fn corners_bunched_up_crossed_or_in_a_line_are_not_a_sheet() {
    assert!(usable_corners(&full(&CORNERS)));
    // placed in any order
    assert!(usable_corners(&[[0.9, 0.1], [0.1, 0.1], [0.9, 0.9], [0.1, 0.9]]));
    // touching
    assert!(!usable_corners(&[[0.5, 0.5], [0.51, 0.5], [0.51, 0.51], [0.5, 0.51]]));
    // in a line
    assert!(!usable_corners(&[[0.1, 0.5], [0.4, 0.5], [0.6, 0.5], [0.9, 0.5]]));
    // one nearly on another's edge
    assert!(!usable_corners(&[[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.5, 0.12]]));
    // dented in
    assert!(!usable_corners(&[[0.1, 0.1], [0.9, 0.1], [0.5, 0.2], [0.1, 0.9]]));
}

/// The corners (px, top left first) of a sheet of size mm seen by a camera tilted down `tilt` and turned `turn` degrees, at
/// `distance` mm.
fn seen(tilt: f64, turn: f64, w: f64, h: f64, focal: f64, size: (f64, f64), distance: f64) -> [P; 4] {
    let (sw, sh) = size;
    let sheet = [[-sw / 2.0, -sh / 2.0, 0.0], [sw / 2.0, -sh / 2.0, 0.0], [sw / 2.0, sh / 2.0, 0.0], [-sw / 2.0, sh / 2.0, 0.0]];
    let (t, u) = (tilt.to_radians(), turn.to_radians());
    let rx = [[1.0, 0.0, 0.0], [0.0, t.cos(), -t.sin()], [0.0, t.sin(), t.cos()]];
    let ry = [[u.cos(), 0.0, u.sin()], [0.0, 1.0, 0.0], [-u.sin(), 0.0, u.cos()]];
    let r = mul(&ry, &rx);
    sheet.map(|p| {
        let v: Vec<f64> = (0..3).map(|i| r[i][0] * p[0] + r[i][1] * p[1] + r[i][2] * p[2]).collect();
        let z = v[2] + distance;
        [focal * v[0] / z + w / 2.0, focal * v[1] / z + h / 2.0]
    })
}

fn view_seen(tilt: f64, turn: f64) -> [P; 4] {
    seen(tilt, turn, 1920.0, 1080.0, FOCAL * 1920.0, (210.0, 297.0), 450.0)
}

fn near(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= b.abs() * rel
}

#[test]
fn the_sheets_real_shape_is_worked_out_from_its_corners() {
    for (tilt, turn) in [(0.0, 0.0), (40.0, 0.0), (60.0, 0.0), (50.0, 10.0), (30.0, 25.0), (60.0, -15.0), (45.0, 30.0)] {
        let got = sheet_aspect(&view_seen(tilt, turn), [960.0, 540.0], FOCAL * 1920.0);
        assert!(near(got, A4, 0.03), "{tilt} {turn}: {got}");
    }
}

#[test]
fn the_shape_holds_up_when_the_focal_length_is_only_guessed_or_corners_are_a_bit_off() {
    // a wider lens than the guess, edges converging only one way
    let wide = seen(45.0, 0.0, 1920.0, 1080.0, 0.65 * 1920.0, (210.0, 297.0), 450.0);
    assert!(near(sheet_aspect(&wide, [960.0, 540.0], FOCAL * 1920.0), A4, 0.06));
    let mut shaky = view_seen(45.0, 20.0);
    for (p, d) in shaky.iter_mut().zip([[3.0, -2.0], [-2.0, 3.0], [2.0, 2.0], [-3.0, -1.0]]) {
        p[0] += d[0];
        p[1] += d[1];
    }
    assert!(near(sheet_aspect(&shaky, [960.0, 540.0], FOCAL * 1920.0), A4, 0.05));
    let landscape = seen(45.0, 0.0, 1920.0, 1080.0, FOCAL * 1920.0, (297.0, 210.0), 450.0);
    assert!(near(sheet_aspect(&landscape, [960.0, 540.0], FOCAL * 1920.0), 1.0 / A4, 0.03));
}

#[test]
fn averaging_the_edges_would_squash_a_sheet_tilted_away() {
    let (sw, sh) = sheet_size(&view_seen(60.0, 0.0));
    // a standing A4 read as landscape: what sheet_aspect fixes
    assert!(sw / sh > 1.1);
}

#[test]
fn a_paper_size_turns_the_way_the_sheet_lies_and_the_stretch_scales_it() {
    let o = |e, s, st| output_aspect(e, s, st);
    assert_eq!(o(0.75, "auto", [100, 100]), Some(0.75));
    assert!(near(o(0.75, "a4", [100, 100]).unwrap(), A4, 1e-9));
    assert!(near(o(1.3, "a4", [100, 100]).unwrap(), 1.0 / A4, 1e-9));
    assert!(near(o(0.6, "letter", [100, 100]).unwrap(), 8.5 / 11.0, 1e-9));
    assert_eq!(o(1.6, "square", [100, 100]), Some(1.0));
    assert_eq!(o(0.75, "fill", [100, 100]), None);
    assert!(near(o(A4, "a4", [150, 100]).unwrap(), A4 * 1.5, 1e-9));
    assert!(near(o(A4, "a4", [100, 200]).unwrap(), A4 / 2.0, 1e-9));
}

#[test]
fn the_sheet_fits_the_frame_in_its_own_shape_centred() {
    let (x, y, rw, rh) = target_rect(Some(A4), 1920, 1080);
    assert_eq!((rh, y), (1080, 0));
    assert_eq!(rw, (1080.0 * A4).round() as usize);
    assert_eq!(x, (1920 - rw) / 2);
    // wider than the frame: bars above and below
    assert_eq!(target_rect(Some(3.0), 1920, 1080), (0, 220, 1920, 640));
    assert_eq!(target_rect(None, W, H), (0, 0, W, H));
}

#[test]
fn a_standing_a4_in_a_wide_frame_keeps_its_shape_instead_of_filling_it() {
    let (w, h) = (640usize, 360usize);
    let corners = seen(55.0, 5.0, w as f64, h as f64, FOCAL * w as f64, (210.0, 297.0), 450.0);
    let matrix = perspective(&[[0.0, 0.0], [210.0, 0.0], [210.0, 297.0], [0.0, 297.0]], &corners);
    let inv = invert(&matrix);
    let mut frame = vec![40u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64, y as f64);
            let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
            let (sx, sy) = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) / d, (inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) / d);
            if (0.0..210.0).contains(&sx) && (0.0..297.0).contains(&sy) {
                frame[(y * w + x) * 3..][..3].fill(240);
            }
        }
    }
    let s = sheet(corners.map(|[x, y]| [x / w as f64, y / h as f64]), "auto");
    let out = straighten(&frame, w, h, &s, &warp_for(&s, w, h));
    let columns: Vec<usize> = (0..w).filter(|x| at(&out, w, *x, h / 2) > 200).collect();
    assert_eq!(out.len(), w * h * 3);
    assert!((0..h).all(|y| (0..20).all(|x| at(&out, w, x, y) == 28 && at(&out, w, w - 1 - x, y) == 28)));
    // not 16:9
    assert!(near((columns[columns.len() - 1] - columns[0] + 1) as f64 / h as f64, A4, 0.04));
    let wider = Sheet { stretch: [120, 100], ..s };
    let out = straighten(&frame, w, h, &wider, &warp_for(&wider, w, h));
    let count = (0..w).filter(|x| at(&out, w, *x, h / 2) > 200).count();
    assert!(near(count as f64, columns.len() as f64 * 1.2, 0.03), "{count}");
}

#[test]
fn straightening_fills_the_sheet_and_puts_the_bar_level_in_the_middle() {
    let out = straight(&desk_frame(), &sheet(full(&CORNERS), "fill"));
    assert_eq!(out.len(), W * H * 3);
    // sheet everywhere near the top, no desk
    assert!((5..20).all(|y| (5..W - 5).all(|x| at(&out, W, x, y) > 200)));
    let dark: Vec<usize> = (0..H).filter(|y| at(&out, W, W / 2, *y) < 100).collect();
    let mean = dark.iter().sum::<usize>() as f64 / dark.len() as f64;
    // the bar, straight across the middle
    assert!((mean - H as f64 / 2.0).abs() < 6.0 && dark.len() < 20, "{mean} {}", dark.len());
    // level: dark the whole way along
    assert!((60..W - 60).all(|x| at(&out, W, x, H / 2) < 100));
}

#[test]
fn a_sheet_that_keeps_its_shape_sits_between_bars() {
    let out = straight(&desk_frame(), &sheet(full(&CORNERS), "auto"));
    assert!((0..H).all(|y| (0..5).all(|x| at(&out, W, x, y) == 28 && at(&out, W, W - 1 - x, y) == 28)));
    assert!(at(&out, W, W / 2, 10) > 200);
}

#[test]
fn corners_stay_on_the_sheet_when_the_view_flips() {
    let flipped_view = Geometry { flip_h: true, ..Geometry::default() };
    let frame = desk_frame();
    let mut flipped = frame.clone();
    for row in flipped.chunks_mut(W * 3) {
        let copy = row.to_vec();
        for x in 0..W {
            row[x * 3..][..3].copy_from_slice(&copy[(W - 1 - x) * 3..][..3]);
        }
    }
    // what the marking on the shown (mirrored) frame comes to in the whole picture
    let mirrored = CORNERS.map(|[x, y]| [W as f64 - x, y]);
    let shown_full: [P; 4] = mirrored.map(|[x, y]| {
        let (fx, fy) = telescope_core::view::shown_to_view(x / W as f64, y / H as f64, &flipped_view);
        [fx, fy]
    });
    let s = Sheet { geometry: flipped_view, ..sheet(shown_full, "fill") };
    let out = straight(&flipped, &s);
    let straight_out = straight(&frame, &sheet(full(&CORNERS), "fill"));
    // the same sheet, mirrored as shown
    let mut diff = 0u64;
    for y in 0..H {
        for x in 0..W {
            diff += (at(&out, W, x, y) as i32 - at(&straight_out, W, W - 1 - x, y) as i32).unsigned_abs() as u64;
        }
    }
    assert!((diff as f64 / (W * H) as f64) < 8.0, "{}", diff as f64 / (W * H) as f64);
}

#[test]
fn clean_up_whitens_shaded_paper_and_black_and_white_is_only_black_and_white() {
    let (w, h) = (200usize, 100usize);
    let mut paper = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let v = if (45..55).contains(&y) && (50..150).contains(&x) { 30 } else { (120.0 + 110.0 * x as f64 / (w - 1) as f64) as u8 };
            paper[(y * w + x) * 3..][..3].fill(v);
        }
    }
    let clean = whiten(&paper, w, h);
    // the dim side came up as much as the bright one
    assert!(at(&clean, w, 5, 10) > 200 && at(&clean, w, 195, 10) > 200);
    assert!(at(&clean, w, 100, 50) < 100);
    let bw = black_and_white(&paper, w, h);
    assert!(bw.iter().all(|v| *v == 0 || *v == 255));
    assert_eq!(at(&bw, w, 100, 50), 0);
    assert_eq!(at(&bw, w, 5, 10), 255);
}

#[test]
fn a_perspective_matrix_takes_each_corner_where_it_was_asked_to() {
    let to = [[0.0, 0.0], [100.0, 0.0], [100.0, 140.0], [0.0, 140.0]];
    let m = perspective(&CORNERS, &to);
    for (c, t) in CORNERS.iter().zip(&to) {
        let d = m[2][0] * c[0] + m[2][1] * c[1] + m[2][2];
        let (x, y) = ((m[0][0] * c[0] + m[0][1] * c[1] + m[0][2]) / d, (m[1][0] * c[0] + m[1][1] * c[1] + m[1][2]) / d);
        assert!((x - t[0]).abs() < 1e-6 && (y - t[1]).abs() < 1e-6, "{x} {y}");
    }
}
