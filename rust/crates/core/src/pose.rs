//! What the vision service says about someone (vision.Person) and the lines of a skeleton (pose.BONES), shared by the plugins that
//! look at people: Fun pack's stick figure, Auto framing and the Preview's skeleton.

use serde_json::Value;
use std::collections::HashMap;

/// Landmark visibility the model has to be this sure of.
pub const SEEN: f64 = 0.5;

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

    /// (x, y) of a landmark, or None when the model isn't sure it's in the picture.
    pub fn point(&self, name: &str, min_visibility: f64) -> Option<(f64, f64)> {
        self.points.get(name).filter(|p| p.2 >= min_visibility).map(|p| (p.0, p.1))
    }
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
}
