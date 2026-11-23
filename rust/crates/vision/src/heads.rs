//! Following people by their faces while the effects are lighter (a face costs about a tenth of a pose). Each face becomes a person with shoulders
//! where the pose model, checking once every BODY_CHECK_S, last saw that head's. The pose model works alone at its usual pace while it sees more
//! people than faces (a group, someone far away) and once no face has been seen for FACE_LOST_S.

use crate::service::{Face, VisionError};
use telescope_core::pose::{Person, SEEN};

/// How often the pose model still runs while faces are followed, to learn how heads sit on shoulders and to count people the face finder misses.
pub const BODY_CHECK_S: f64 = 1.0;
/// With no face for this long the pose model does it alone, until a face is back.
pub const FACE_LOST_S: f64 = 0.3;
/// Shoulder width and how far the shoulders are under the eyes, in face widths, until the pose model has been seen with a face.
pub const SHOULDERS_PER_FACE: f64 = 1.7;
pub const SHOULDERS_UNDER_EYES: f64 = 1.3;

/// One stream's faces turned into people.
pub struct Heads {
    shoulders: f64,
    drop: f64,
    learned: u32,
    body_at: f64,
    face_at: f64,
    crowd: bool,
    last: Vec<Person>,
}

impl Default for Heads {
    fn default() -> Self {
        Heads::new()
    }
}

impl Heads {
    pub fn new() -> Heads {
        Heads { shoulders: SHOULDERS_PER_FACE, drop: SHOULDERS_UNDER_EYES, learned: 0, body_at: f64::MIN, face_at: f64::MIN, crowd: false, last: Vec::new() }
    }

    /// Faces are followed again after a spell of whole bodies: the next frame checks the body at once. What was learned of the shoulders stays.
    pub fn restart(&mut self) {
        (self.body_at, self.face_at, self.crowd) = (f64::MIN, f64::MIN, false);
        self.last.clear();
    }

    /// The shoulder width and drop learned so far, in face widths.
    pub fn shape(&self) -> (f64, f64) {
        (self.shoulders, self.drop)
    }

    /// People for a frame with these faces, running `pose` for the check or every `pose_gap` while it works alone; None while it rests between those runs.
    pub fn people(&mut self, faces: &[Face], aspect: f64, now: f64, pose_gap: f64, pose: &mut dyn FnMut() -> Result<Vec<Person>, VisionError>) -> Result<Option<Vec<Person>>, VisionError> {
        if !faces.is_empty() {
            self.face_at = now;
        }
        let alone = self.crowd || (faces.is_empty() && now - self.face_at >= FACE_LOST_S);
        let due = now - self.body_at >= if alone { pose_gap } else { BODY_CHECK_S };
        if due {
            self.body_at = now;
            let bodies = pose()?;
            self.learn(faces, &bodies, aspect);
            if self.crowd || faces.is_empty() {
                self.last = bodies.clone();
                return Ok(Some(bodies));
            }
        } else if alone {
            return Ok(None);
        }
        if faces.is_empty() {
            // A face missed for a moment (a turn of the head) keeps its person
            return Ok(Some(self.last.clone()));
        }
        self.last = faces.iter().map(|f| self.person(f, aspect)).collect();
        Ok(Some(self.last.clone()))
    }

    /// Takes in how one head sits on its shoulders, and whether the pose model sees anyone the face finder doesn't.
    fn learn(&mut self, faces: &[Face], bodies: &[Person], aspect: f64) {
        // With no face at all it's the face that's lost, not a group: that ends as soon as a face is back
        if !faces.is_empty() {
            self.crowd = bodies.len() > faces.len();
        }
        let ([face], [body]) = (faces, bodies) else { return };
        let (Some(l), Some(r)) = (body.point("left_shoulder", SEEN), body.point("right_shoulder", SEEN)) else { return };
        let width = (face.area.2 - face.area.0) * aspect;
        let eyes = ["left_eye", "right_eye"].iter().filter_map(|n| body.point(n, SEEN)).map(|p| p.1).reduce(f64::min);
        let Some(eyes) = eyes.filter(|_| width > 1e-3) else { return };
        let rate = if self.learned < 3 { 0.5 } else { 0.3 };
        self.learned += 1;
        let shoulders = ((l.0 - r.0) * aspect).hypot(l.1 - r.1) / width;
        let drop = ((l.1 + r.1) / 2.0 - eyes) / width;
        self.shoulders += (shoulders.clamp(0.8, 3.0) - self.shoulders) * rate;
        self.drop += (drop.clamp(0.6, 2.2) - self.drop) * rate;
    }

    /// A face as a person: its head points, and shoulders where this head's were last seen.
    fn person(&self, face: &Face, aspect: f64) -> Person {
        let width = (face.area.2 - face.area.0) * aspect;
        let eyes = face.left_eye.1.min(face.right_eye.1);
        let (cx, sy, half) = ((face.area.0 + face.area.2) / 2.0, eyes + self.drop * width, self.shoulders * width / 2.0 / aspect);
        let sure = face.score.clamp(SEEN, 1.0);
        Person::new([
            ("nose", face.nose.0, face.nose.1, sure),
            ("left_eye", face.left_eye.0, face.left_eye.1, sure),
            ("right_eye", face.right_eye.0, face.right_eye.1, sure),
            ("left_ear", face.left_ear.0, face.left_ear.1, sure),
            ("right_ear", face.right_ear.0, face.right_ear.1, sure),
            // The person's left is on the right of the picture, as the pose model has it
            ("left_shoulder", cx + half, sy, sure),
            ("right_shoulder", cx - half, sy, sure),
        ])
    }
}

#[cfg(test)]
#[path = "heads_tests.rs"]
mod tests;
