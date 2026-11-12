//! core::view against what telescope/view.py works out on the same numbers (recorded from the Python app): how the zoom is shared
//! between the phone and this computer, the points mapped between the picture as shown and the phone's frame, the lens boxes.

use serde_json::Value;
use telescope_core::view::*;

fn data() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/view.json");
    telescope_core::json::loads(&std::fs::read_to_string(path).expect("view.json")).expect("json")
}

fn f(v: &Value) -> f64 {
    v.as_f64().expect("a number")
}

fn pair(v: &Value) -> (f64, f64) {
    (f(&v[0]), f(&v[1]))
}

fn caps_of(data: &Value, name: &str) -> Option<PhoneZoomCaps> {
    let c = &data["caps"][name];
    (!c.is_null()).then(|| PhoneZoomCaps {
        ratio_max: f(&c["ratio_max"]), crop_max: f(&c["crop_max"]), freeform: c["freeform"] == true,
        lens_zooms: c["lens_zooms"].as_array().unwrap().iter().map(f).collect(),
    })
}

fn near(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-9, "{what}: {a} against {b}");
}

fn geometry(row: &Value) -> Geometry {
    let (pan_x, pan_y) = pair(&row["pan"]);
    let (stretch_x, stretch_y) = pair(&row["stretch"]);
    Geometry {
        zoom: f(&row["zoom"]), pan_x, pan_y, flip_h: row["flip_h"] == true, flip_v: row["flip_v"] == true,
        rotation: row["rotation"].as_i64().map(|r| r as i32), stretch_x, stretch_y, distortion: 0.0,
    }
}

#[test]
fn the_zoom_is_split_between_phone_and_computer_as_python_does() {
    let data = data();
    for row in data["splits"].as_array().unwrap() {
        let caps = caps_of(&data, row["caps"].as_str().unwrap());
        let (pan_x, pan_y) = pair(&row["pan"]);
        let split = split_zoom(f(&row["zoom"]), pan_x, pan_y, caps.as_ref(), f(&row["current"]));
        let what = format!("{row}");
        match (split.phone, row["phone"].as_array()) {
            (None, None) => {}
            (Some(p), Some(want)) => {
                for (got, want) in [p.ratio, p.crop, p.x, p.y].into_iter().zip(want) {
                    near(got, f(want), &what);
                }
            }
            (got, want) => panic!("{what}: {got:?} against {want:?}"),
        }
        let want = row["desktop"].as_array().unwrap();
        for (got, want) in [split.desktop.0, split.desktop.1, split.desktop.2].into_iter().zip(want) {
            near(got, f(want), &what);
        }
    }
}

#[test]
fn points_map_between_the_picture_as_shown_and_the_phone_as_python_does() {
    for row in data()["points"].as_array().unwrap() {
        let g = geometry(row);
        let (u, v) = pair(&row["at"]);
        let what = format!("{row}");
        let (x, y) = shown_to_view(u, v, &g);
        let want = pair(&row["to_view"]);
        near(x, want.0, &what);
        near(y, want.1, &what);
        let (x, y) = view_to_shown(u, v, &g);
        let want = pair(&row["to_shown"]);
        near(x, want.0, &what);
        near(y, want.1, &what);
        let (x, y) = inverse_map(u, v, 1600.0, 900.0, &g);
        let want = pair(&row["inverse"]);
        near(x, want.0, &what);
        near(y, want.1, &what);
    }
}

#[test]
fn lens_boxes_zoom_steps_and_pans_match_python() {
    let data = data();
    for row in data["lens_boxes"].as_array().unwrap() {
        let caps = caps_of(&data, row["caps"].as_str().unwrap());
        let (pan_x, pan_y) = pair(&row["pan"]);
        let g = Geometry { zoom: f(&row["zoom"]), pan_x, pan_y, rotation: row["rotation"].as_i64().map(|r| r as i32), ..Geometry::default() };
        let got = lens_boxes(caps.as_ref(), &g);
        let want = row["boxes"].as_array().unwrap();
        assert_eq!(got.len(), want.len());
        for ((label, x0, y0, x1, y1), want) in got.into_iter().zip(want) {
            assert_eq!(label, want[0].as_str().unwrap());
            for (got, want) in [x0, y0, x1, y1].into_iter().zip(&want.as_array().unwrap()[1..]) {
                near(got, f(want), &row.to_string());
            }
        }
    }
    for row in data["pan_for"].as_array().unwrap() {
        near(pan_for(f(&row[0]), f(&row[1])), f(&row[2]), &row.to_string());
    }
    for row in data["zoom_detent"].as_array().unwrap() {
        let marks: Vec<i32> = row[2].as_array().unwrap().iter().map(|m| m.as_i64().unwrap() as i32).collect();
        assert_eq!(zoom_detent(row[0].as_i64().unwrap() as i32, row[1].as_i64().unwrap() as i32, &marks) as i64, row[3].as_i64().unwrap());
    }
    for row in data["lens_step"].as_array().unwrap() {
        assert_eq!(lens_step(f(&row[0])) as i64, row[1].as_i64().unwrap());
    }
    let (x, y) = inverse_map(1.2, -0.1, 0.0, 0.0, &Geometry { zoom: 2.0, ..Geometry::default() });
    assert_eq!((x, y), (f(&data["unknown_size"][0]), f(&data["unknown_size"][1])));
}
