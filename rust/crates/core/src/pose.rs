//! What the vision service says about someone (vision.Person) and the lines of a skeleton (pose.BONES), shared by the plugins that
//! look at people: Fun pack's stick figure, Auto framing and the Preview's skeleton. Points are (x, y) in the picture as shown at
//! zoom 1, as Person has them.

use serde_json::Value;
use std::collections::HashMap;

/// Landmark visibility the model has to be this sure of.
pub const SEEN: f64 = 0.5;

/// Arms count at lower visibility: past the edge of the picture the model only guesses where they are.
pub const ARM_SEEN: f64 = 0.15;
/// A wrist this many shoulder widths out past its shoulder is holding something out.
pub const HAND_OUT: f64 = 0.9;
/// Or an elbow this far out with the wrist further still (the hand itself can be out of the picture).
pub const ELBOW_OUT: f64 = 0.55;
pub const SIDES: [&str; 2] = ["left", "right"];

/// The lines of a skeleton, between a Person's point names.
pub const BONES: [(&str, &str); 22] = [
    ("left_ear", "left_eye"), ("left_eye", "nose"), ("nose", "right_eye"), ("right_eye", "right_ear"),
    ("left_shoulder", "right_shoulder"), ("left_hip", "right_hip"),
    ("left_shoulder", "left_hip"), ("right_shoulder", "right_hip"),
    ("left_hip", "left_knee"), ("left_knee", "left_ankle"), ("right_hip", "right_knee"), ("right_knee", "right_ankle"),
    ("left_shoulder", "left_elbow"), ("left_elbow", "left_wrist"),
    ("left_wrist", "left_index"), ("left_wrist", "left_pinky"), ("left_wrist", "left_thumb"),
    ("right_shoulder", "right_elbow"), ("right_elbow", "right_wrist"),
    ("right_wrist", "right_index"), ("right_wrist", "right_pinky"), ("right_wrist", "right_thumb"),
];

/// One person the pose model found: name -> (x, y, visibility), x and y 0..1 of the picture as shown at zoom 1 (upright, flipped
/// the way the view is).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Person {
    pub points: HashMap<String, (f64, f64, f64)>,
}

impl Person {
    /// A person from the event's record: {"points": {"nose": [x, y, visibility], ...}}, or the points alone.
    pub fn from_value(value: &Value) -> Option<Person> {
        let points = value.get("points").unwrap_or(value).as_object()?;
        let mut out = HashMap::new();
        for (name, p) in points {
            let p = p.as_array()?;
            if p.len() == 3 {
                out.insert(name.clone(), (p[0].as_f64()?, p[1].as_f64()?, p[2].as_f64()?));
            }
        }
        Some(Person { points: out })
    }

    /// A person from (name, x, y, visibility) points.
    pub fn new<'a>(points: impl IntoIterator<Item = (&'a str, f64, f64, f64)>) -> Person {
        Person { points: points.into_iter().map(|(n, x, y, v)| (n.to_string(), (x, y, v))).collect() }
    }

    /// The record people_seen carries: {"points": {"nose": [x, y, visibility], ...}}, names sorted.
    pub fn to_value(&self) -> Value {
        let mut names: Vec<&String> = self.points.keys().collect();
        names.sort();
        let points: serde_json::Map<String, Value> = names.into_iter().map(|n| {
            let p = self.points[n];
            (n.clone(), serde_json::json!([p.0, p.1, p.2]))
        }).collect();
        serde_json::json!({"points": points})
    }

    /// The same person with each (x, y) put through to(x, y).
    pub fn mapped(&self, to: &dyn Fn(f64, f64) -> (f64, f64)) -> Person {
        Person { points: self.points.iter().map(|(n, &(x, y, seen))| {
            let (x, y) = to(x, y);
            (n.clone(), (x, y, seen))
        }).collect() }
    }

    /// (x, y) of a landmark, or None when the model isn't sure it's in the picture.
    pub fn point(&self, name: &str, min_visibility: f64) -> Option<(f64, f64)> {
        self.points.get(name).filter(|p| p.2 >= min_visibility).map(|p| (p.0, p.1))
    }
}

fn mid(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

/// (shoulder centre, shoulder width in picture heights) or None without both shoulders.
pub fn body(person: &Person, aspect: f64) -> Option<((f64, f64), f64)> {
    let (left, right) = (person.point("left_shoulder", SEEN)?, person.point("right_shoulder", SEEN)?);
    let width = ((left.0 - right.0) * aspect).hypot(left.1 - right.1);
    (width > 1e-3).then(|| (mid(left, right), width))
}

/// Maps a point of an upright pose back to the picture as shown.
pub type ToShown = Box<dyn Fn(f64, f64) -> (f64, f64) + Send + Sync>;

/// The pose with its shoulders horizontal and head above them, plus a mapping back to the shown picture. Works in picture
/// heights so a quarter turn on a non-square frame keeps distances.
pub fn upright(person: &Person, aspect: f64) -> (Person, ToShown) {
    let Some(((cx, cy), width)) = body(person, aspect) else {
        return (person.clone(), Box::new(|x, y| (x, y)));
    };
    let (left, right) = (person.point("left_shoulder", SEEN).unwrap(), person.point("right_shoulder", SEEN).unwrap());
    let across = ((left.0 - right.0) * aspect / width, (left.1 - right.1) / width);
    let mut down = (-across.1, across.0);
    let head: Vec<(f64, f64)> = ["nose", "left_eye", "right_eye"].iter().filter_map(|n| person.point(n, SEEN)).collect();
    let hips: Vec<(f64, f64)> = ["left_hip", "right_hip"].iter().filter_map(|n| person.point(n, SEEN)).collect();
    if !head.is_empty() || !hips.is_empty() {
        let points = if head.is_empty() { &hips } else { &head };
        let n = points.len() as f64;
        let dx = (points.iter().map(|p| p.0).sum::<f64>() / n - cx) * aspect;
        let dy = points.iter().map(|p| p.1).sum::<f64>() / n - cy;
        if (dx * down.0 + dy * down.1) * if head.is_empty() { 1.0 } else { -1.0 } < 0.0 {
            down = (-down.0, -down.1);
        }
    } else if down.1 < 0.0 {
        down = (-down.0, -down.1);
    }
    let across = (down.1, -down.0);
    let to_upright = move |x: f64, y: f64| {
        let (dx, dy) = ((x - cx) * aspect, y - cy);
        (cx + (dx * across.0 + dy * across.1) / aspect, cy + dx * down.0 + dy * down.1)
    };
    let to_shown = move |x: f64, y: f64| {
        let (dx, dy) = ((x - cx) * aspect, y - cy);
        (cx + (dx * across.0 + dy * down.0) / aspect, cy + dx * across.1 + dy * down.1)
    };
    (person.mapped(&to_upright), Box::new(to_shown))
}

/// One arm's measures (pose.arm): the shoulder width, the shoulder it hangs from and the line below which the hand is low.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arm {
    pub width: f64,
    pub shoulder: (f64, f64),
    pub away: f64,
    pub aspect: f64,
    pub below: f64,
}

impl Arm {
    /// How far out past the shoulder a point is.
    pub fn out(&self, p: (f64, f64)) -> f64 {
        (p.0 - self.shoulder.0) * self.away * self.aspect
    }
}

/// One arm's measures, or None without both shoulders.
pub fn arm(person: &Person, side: &str, aspect: f64) -> Option<Arm> {
    let ((cx, sy), width) = body(person, aspect)?;
    let shoulder = person.point(&format!("{side}_shoulder"), SEEN)?;
    let away = if shoulder.0 >= cx { 1.0 } else { -1.0 };
    let below = match person.point(&format!("{side}_hip"), SEEN) {
        Some(hip) => hip.1 - 0.2 * width,
        None => sy + 1.4 * width,
    };
    Some(Arm { width, shoulder, away, aspect, below })
}

/// The sides ("left", "right") on which someone holds a hand out (showing something), above their hips.
pub fn hands_out(people: &[Person], aspect: f64) -> Vec<&'static str> {
    let mut found = Vec::new();
    for person in people {
        let (person, _to_shown) = upright(person, aspect);
        for side in SIDES {
            let Some(a) = arm(&person, side, aspect) else { continue };
            let elbow = person.point(&format!("{side}_elbow"), ARM_SEEN);
            let Some(wrist) = person.point(&format!("{side}_wrist"), ARM_SEEN) else { continue };
            if wrist.1 > a.below {
                continue;
            }
            let out = a.out(wrist) > HAND_OUT * a.width || elbow.is_some_and(|e| a.out(e) > ELBOW_OUT * a.width && a.out(wrist) > a.out(e));
            if out && !found.contains(&side) {
                found.push(side);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_person_reads_from_its_record_and_hides_what_it_isnt_sure_of() {
        let p = Person::from_value(&json!({"points": {"nose": [0.5, 0.25, 0.9], "left_hip": [0.4, 0.7, 0.1]}})).unwrap();
        assert_eq!(p.point("nose", SEEN), Some((0.5, 0.25)));
        assert_eq!(p.point("left_hip", SEEN), None);
        assert_eq!(p.point("left_hip", 0.05), Some((0.4, 0.7)));
        assert_eq!(p.point("right_hip", SEEN), None);
        assert_eq!(Person::from_value(&json!({"nose": [0.1, 0.2, 1.0]})).unwrap().points.len(), 1);
        assert!(Person::from_value(&json!(7)).is_none());
        assert_eq!(BONES.len(), 22);
    }

    #[test]
    fn a_tilted_pose_is_turned_upright_and_maps_back() {
        let p = Person::new([("left_shoulder", 0.6, 0.6, 1.0), ("right_shoulder", 0.4, 0.4, 1.0), ("nose", 0.4, 0.6, 1.0)]);
        let (up, back) = upright(&p, 1.0);
        let (l, r) = (up.point("left_shoulder", SEEN).unwrap(), up.point("right_shoulder", SEEN).unwrap());
        assert!((l.1 - r.1).abs() < 1e-9);
        assert!(up.point("nose", SEEN).unwrap().1 < l.1);
        let (x, y) = back(l.0, l.1);
        assert!((x - 0.6).abs() < 1e-9 && (y - 0.6).abs() < 1e-9);
        assert_eq!(Person::from_value(&p.to_value()), Some(p));
    }

    #[test]
    fn a_hand_held_out_is_seen_on_its_side() {
        let mut p = Person::new([("left_shoulder", 0.55, 0.4, 1.0), ("right_shoulder", 0.45, 0.4, 1.0), ("nose", 0.5, 0.3, 1.0),
                                 ("left_hip", 0.53, 0.75, 1.0), ("right_hip", 0.47, 0.75, 1.0), ("left_wrist", 0.75, 0.45, 0.9),
                                 ("right_wrist", 0.46, 0.7, 0.9)]);
        assert_eq!(hands_out(std::slice::from_ref(&p), 1.0), ["left"]);
        p.points.insert("left_wrist".into(), (0.75, 0.9, 0.9));
        assert!(hands_out(&[p], 1.0).is_empty());
    }
}
