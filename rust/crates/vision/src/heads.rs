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
/// A face is a body's head when it is within this many shoulder widths of the shoulders' middle, sideways.
const HEAD_REACH: f64 = 0.75;
/// A body's own head points are on its face when they are within this many face widths of the face's middle.
const HEAD_ON_FACE: f64 = 1.0;

/// One stream's faces turned into people.
pub struct Heads {
    shoulders: f64,
    drop: f64,
    /// How far the middle of the shoulders sits to the right of the face's middle, in face widths.
    offset: f64,
    learned: u32,
    body_at: f64,
    face_at: f64,
    crowd: bool,
    alone: bool,
    /// Whether the people last given came from faces (a face missed for a moment included), not from the pose model.
    faces: bool,
    last: Vec<Person>,
}

impl Default for Heads {
    fn default() -> Self {
        Heads::new()
    }
}

impl Heads {
    pub fn new() -> Heads {
        Heads { shoulders: SHOULDERS_PER_FACE, drop: SHOULDERS_UNDER_EYES, offset: 0.0, learned: 0, body_at: f64::MIN, face_at: f64::MIN, crowd: false, alone: false, faces: false, last: Vec::new() }
    }

    /// Faces are followed again after a spell of whole bodies: the next frame checks the body at once. What was learned of the shoulders stays.
    pub fn restart(&mut self) {
        (self.body_at, self.face_at, self.crowd, self.alone, self.faces) = (f64::MIN, f64::MIN, false, false, false);
        self.last.clear();
    }

    /// The picture's turn changed under what was seen: landmarks from before it mean nothing now, so the next frame checks the body. A mirrored picture has the shoulders on the other side of the face, a turned one on some other.
    pub fn turned(&mut self, mirrored: bool, rotated: bool) {
        self.restart();
        if rotated {
            self.offset = 0.0;
        } else if mirrored {
            self.offset = -self.offset;
        }
    }

    /// The shoulder width and drop learned so far, in face widths.
    pub fn shape(&self) -> (f64, f64) {
        (self.shoulders, self.drop)
    }

    /// Whether the last people given came from faces, not from the pose model: only then are faces what is followed.
    pub fn by_faces(&self) -> bool {
        self.faces
    }

    /// Whether the pose model works alone as of the last frame (a group, or no face for a moment): frames are wanted at its own pace.
    pub fn alone(&self) -> bool {
        self.alone
    }

    /// People for a frame with these faces, running `pose` for the check or every `pose_gap` while it works alone; None while it rests between those runs.
    pub fn people(&mut self, faces: &[Face], aspect: f64, now: f64, pose_gap: f64, pose: &mut dyn FnMut() -> Result<Vec<Person>, VisionError>) -> Result<Option<Vec<Person>>, VisionError> {
        if !faces.is_empty() {
            self.face_at = now;
        }
        let lost = faces.is_empty() && now - self.face_at >= FACE_LOST_S;
        self.alone = self.crowd || lost;
        self.faces = false;
        // Frames of a pose model working alone come as often as its gap, a little early now and then
        let due = now - self.body_at >= if self.alone { pose_gap * 0.9 } else { BODY_CHECK_S };
        if due {
            self.body_at = now;
            let bodies = pose()?;
            self.learn(faces, &bodies, aspect);
            self.alone = self.crowd || lost;
            if self.crowd || faces.is_empty() {
                self.last = bodies.clone();
                return Ok(Some(bodies));
            }
        } else if self.alone {
            return Ok(None);
        }
        self.faces = true;
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
            // Someone the pose model sees without a face of theirs (a group, someone turned away, someone else's face in the way) means faces can't be followed
            self.crowd = faceless(faces, bodies, aspect) > 0;
        }
        let ([face], [body]) = (faces, bodies) else { return };
        if !head_of(face, body, aspect) {
            return;
        }
        let (Some(l), Some(r)) = (body.point("left_shoulder", SEEN), body.point("right_shoulder", SEEN)) else { return };
        let width = (face.area.2 - face.area.0) * aspect;
        let eyes = ["left_eye", "right_eye"].iter().filter_map(|n| body.point(n, SEEN)).map(|p| p.1).reduce(f64::min);
        let Some(eyes) = eyes.filter(|_| width > 1e-3) else { return };
        let rate = if self.learned < 3 { 0.5 } else { 0.3 };
        let shoulders = ((l.0 - r.0) * aspect).hypot(l.1 - r.1) / width;
        let drop = ((l.1 + r.1) / 2.0 - eyes) / width;
        let offset = ((l.0 + r.0) / 2.0 - (face.area.0 + face.area.2) / 2.0) * aspect / width;
        self.shoulders += (shoulders.clamp(0.8, 3.0) - self.shoulders) * rate;
        self.drop += (drop.clamp(0.6, 2.2) - self.drop) * rate;
        // The first sighting is taken whole, so the shoulders don't slide over from the middle of the face when the models take turns
        let rate = if self.learned == 0 { 1.0 } else { rate };
        self.offset += (offset.clamp(-1.2, 1.2) - self.offset) * rate;
        self.learned += 1;
    }

    /// A face as a person: its head points, and shoulders where this head's were last seen.
    fn person(&self, face: &Face, aspect: f64) -> Person {
        let width = (face.area.2 - face.area.0) * aspect;
        let eyes = face.left_eye.1.min(face.right_eye.1);
        let (cx, sy, half) = ((face.area.0 + face.area.2) / 2.0 + self.offset * width / aspect, eyes + self.drop * width, self.shoulders * width / 2.0 / aspect);
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

/// Whether this face is this body's head: on the body's own head points when the pose model sees any, else over its shoulders (above the line they make and not far to either side).
fn head_of(face: &Face, body: &Person, aspect: f64) -> bool {
    let (fx, fy) = ((face.area.0 + face.area.2) / 2.0, (face.area.1 + face.area.3) / 2.0);
    let width = (face.area.2 - face.area.0) * aspect;
    let head: Vec<(f64, f64)> = ["nose", "left_eye", "right_eye"].iter().filter_map(|n| body.point(n, SEEN)).collect();
    if !head.is_empty() {
        let n = head.len() as f64;
        let (hx, hy) = (head.iter().map(|p| p.0).sum::<f64>() / n, head.iter().map(|p| p.1).sum::<f64>() / n);
        return ((hx - fx) * aspect).hypot(hy - fy) <= HEAD_ON_FACE * width;
    }
    let Some(((mx, my), shoulders)) = telescope_core::pose::body(body, aspect) else { return false };
    ((fx - mx) * aspect).abs() <= HEAD_REACH * shoulders && fy < my
}

/// How many bodies have no face of their own among these faces, each face going to one body.
fn faceless(faces: &[Face], bodies: &[Person], aspect: f64) -> usize {
    let mut free: Vec<&Face> = faces.iter().collect();
    bodies
        .iter()
        .filter(|body| match free.iter().position(|f| head_of(f, body, aspect)) {
            Some(i) => {
                free.swap_remove(i);
                false
            }
            None => true,
        })
        .count()
}

#[cfg(test)]
#[path = "heads_tests.rs"]
mod tests;
