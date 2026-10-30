//! People in the picture as a shared host service (vision.py's Vision, vision_download.py and vision_status.py). The vision
//! crate implements VisionService and the binary hands it to the shell in Services; plugins never see it. They get:
//! - FrameContext::person_mask in a frame step (Background, Privacy blur, Fun pack's cloak),
//! - HostVision::want_people, then Event::PeopleSeen (Person records, Auto framing) and Event::PeopleTracked (Track records
//!   from core::people, Privacy blur and Expose for my face), or HostVision::tracked_people,
//! - HostVision::vision_state and vision_pack, with status_text for the card's status row and download_vision for its button.

use crate::bus::BusSender;
use crate::frame::Frame;
use crate::i18n::tr;
use crate::view::{shown_to_view, view_to_shown, Geometry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// What vision_state() reports: nothing asked yet.
pub const OFF: &str = "off";
pub const LOADING: &str = "loading";
pub const READY: &str = "ready";
/// The vision pack isn't downloaded.
pub const MISSING: &str = "missing";
/// MediaPipe didn't load; VisionService::diagnostics says why.
pub const FAILED: &str = "failed";

/// The view's flips and turn (degrees clockwise), the part of it that moves people around the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub flip_h: bool,
    pub flip_v: bool,
    pub rotation: Option<i32>,
}

impl Turn {
    fn geometry(&self, zoom: f64, pan_x: f64, pan_y: f64) -> Geometry {
        Geometry { zoom, pan_x, pan_y, flip_h: self.flip_h, flip_v: self.flip_v, rotation: self.rotation, stretch_x: 1.0, stretch_y: 1.0 }
    }
}

/// A point in the picture as shown at zoom 1 with turn `old` to where it is with turn `new` (the remap app.py and Auto framing do
/// when the view is flipped or turned).
pub fn turned(old: Turn, new: Turn) -> impl Fn(f64, f64) -> (f64, f64) {
    move |x, y| {
        let (fx, fy) = shown_to_view(x, y, &old.geometry(1.0, 0.0, 0.0));
        view_to_shown(fx, fy, &new.geometry(1.0, 0.0, 0.0))
    }
}

/// How a stream's raw frame (before the view) is turned the way it's shown for the pose model, and where its points land in the
/// picture as shown at zoom 1, through the phone's share of the zoom it was sent with (view_controller._upright).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Upright {
    pub turn: Turn,
    /// (zoom, pan_x, pan_y) the phone already did (view.phone_window); (1, 0, 0) for none.
    pub sent: (f64, f64, f64),
}

impl Default for Upright {
    fn default() -> Self {
        Upright { turn: Turn::default(), sent: (1.0, 0.0, 0.0) }
    }
}

impl Upright {
    /// A point of the turned frame to the picture as shown at zoom 1.
    pub fn to_shown(&self, u: f64, v: f64) -> (f64, f64) {
        let (x, y) = shown_to_view(u, v, &self.turn.geometry(self.sent.0, self.sent.1, self.sent.2));
        view_to_shown(x, y, &self.turn.geometry(1.0, 0.0, 0.0))
    }
}

/// Where the vision pack's download is (VisionDownload), for the cards' status rows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VisionPack {
    /// This version's pack is unpacked and checked.
    pub ready: bool,
    /// There's a pack for this computer at all.
    pub available: bool,
    pub downloading: bool,
    /// About how big the download is, in MB (at least 1).
    pub size_mb: u64,
    /// Bytes so far and in all (0: not known) while it downloads.
    pub done: u64,
    pub size: u64,
    /// What went wrong last time, and the optional plugin that was waiting for it.
    pub error: String,
    pub failed_for: String,
}

/// The status row on a card that uses vision (VisionStatus._show and its progress): (text, whether Download shows). Hidden
/// while the text is empty. active: the card is switched to something that needs vision; problem: the card's own reason,
/// shown first; optional: it works without vision too, only better with it.
pub fn status_text(active: bool, problem: &str, optional: bool, state: &str, pack: &VisionPack) -> (String, bool) {
    let mb = pack.size_mb.max(1).to_string();
    if !active {
        (String::new(), false)
    } else if !problem.is_empty() {
        (problem.to_string(), false)
    } else if pack.downloading {
        let text = if let Some(percent) = (pack.done * 100).checked_div(pack.size) {
            tr("Downloading… {percent}%").replace("{percent}", &percent.min(99).to_string())
        } else {
            tr("Downloading…")
        };
        (text, false)
    } else if state == MISSING && optional {
        (tr("Cleaner edges with a one-time download (about {mb} MB).").replace("{mb}", &mb), pack.available)
    } else if state == MISSING {
        (tr("Needs a one-time download (about {mb} MB).").replace("{mb}", &mb), pack.available)
    } else if optional {
        (String::new(), false)
    } else if state == FAILED {
        (tr("MediaPipe didn't load on this computer. Copy diagnostics has the details."), false)
    } else if state == LOADING {
        (tr("Getting ready…"), false)
    } else {
        (String::new(), false)
    }
}

/// The vision service the shell runs, one for the whole app. Every call is safe before it's ready (None, nothing).
pub trait VisionService: Send + Sync {
    /// Where people_seen, people_tracked, vision_state_changed and the download's events go from now on.
    fn attach(&self, bus: BusSender);
    /// OFF until something asks, then LOADING and READY; MISSING (without loading) when there's no pack; FAILED.
    fn state(&self) -> String;
    /// Start loading in the background, once; after no pack was found, again every few seconds.
    fn load(&self);
    /// From a frame step: how sure it is each pixel of source_id's frame is a person (0..255, the frame's size), or None
    /// while it can't say. The same frame (by number) asked twice is worked out once.
    fn person_mask(&self, source_id: &str, frame: &Frame) -> Option<Arc<Vec<u8>>>;
    /// While any owner wants people, fed frames are looked at and people_seen and people_tracked follow.
    fn want_people(&self, owner: &str, on: bool);
    fn wants_people(&self) -> bool;
    /// A stream's frame before the view, from its thread; most are skipped (about 12 a second go on while something moves).
    fn feed_raw(&self, source_id: &str, frame: &Frame, upright: Upright);
    /// (Track records, how many people) on source_id's stream as the last people_tracked said; ([], 0) when stale.
    fn tracked_people(&self, source_id: &str) -> (Vec<Value>, u32);
    /// The view was flipped or turned: everyone tracked moves with it and keeps their id.
    fn view_turned(&self, old: Turn, new: Turn);
    /// A stream ended: its models close, its people thread ends and its people are forgotten.
    fn drop_stream(&self, source_id: &str);
    /// Power-saving effects (Settings > Video): the models run less often.
    fn set_power_saving(&self, on: bool);
    /// {"Vision": "..."} for Copy diagnostics; empty while OFF.
    fn diagnostics(&self) -> Vec<(String, String)>;
    /// No callback reaches the bus once this returns.
    fn shutdown(&self);
    /// The download, for the cards' status rows and the Plugins tab.
    fn pack(&self) -> VisionPack;
    /// Download in the background, then vision_download_finished names switch_on (the optional plugin to switch on, or "").
    fn download(&self, switch_on: &str);
    fn cancel_download(&self);
    /// Delete the downloaded packs.
    fn remove_pack(&self);
    /// What went wrong after the download itself, shown like its own failure on the waiting plugin's tile.
    fn download_failed(&self, _error: &str, _plugin: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mirror_moves_people_across_and_back() {
        let flip = turned(Turn::default(), Turn { flip_h: true, ..Turn::default() });
        let (x, y) = flip(0.3, 0.4);
        assert!((x - 0.7).abs() < 1e-12 && (y - 0.4).abs() < 1e-12);
        let up = Upright { turn: Turn { rotation: Some(90), ..Turn::default() }, sent: (1.0, 0.0, 0.0) };
        assert_eq!(up.to_shown(0.25, 0.5), (0.25, 0.5));
    }

    #[test]
    fn the_status_row_says_why_nothing_happens_yet() {
        let pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
        assert_eq!(status_text(false, "", false, MISSING, &pack), (String::new(), false));
        assert_eq!(status_text(true, "Mine", false, MISSING, &pack), ("Mine".into(), false));
        assert_eq!(status_text(true, "", false, MISSING, &pack), ("Needs a one-time download (about 47 MB).".into(), true));
        assert_eq!(status_text(true, "", true, MISSING, &pack).0, "Cleaner edges with a one-time download (about 47 MB).");
        assert_eq!(status_text(true, "", true, FAILED, &pack).0, "");
        assert_eq!(status_text(true, "", false, LOADING, &pack).0, "Getting ready…");
        assert_eq!(status_text(true, "", false, READY, &pack).0, "");
        let going = VisionPack { downloading: true, done: 50, size: 100, ..pack };
        assert_eq!(status_text(true, "", false, MISSING, &going).0, "Downloading… 50%");
    }
}
