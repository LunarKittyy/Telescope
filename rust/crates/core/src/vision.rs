//! People in the picture as a shared host service (vision.py's Vision, vision_download.py and vision_status.py). The vision
//! crate implements VisionService and the binary hands it to the shell in Services; plugins never see it. They get:
//! - FrameContext::person_mask in a frame step (Background, Privacy blur, Fun pack's cloak), or person_mask_at for one that works at a smaller size,
//! - HostVision::want_people, then Event::PeopleSeen (Person records, Auto framing) and Event::PeopleTracked (Track records
//!   from core::people, Privacy blur and Expose for my face), or HostVision::tracked_people,
//! - HostVision::vision_state and vision_pack, with status_text for the card's status row and download_vision for its button.

use crate::i18n::Fields;
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
            tr("Downloading… {percent}%").field("percent", percent.min(99).to_string())
        } else {
            tr("Downloading…")
        };
        (text, false)
    } else if state == MISSING && optional {
        (tr("Cleaner edges with a one-time download (about {mb} MB).").field("mb", &mb), pack.available)
    } else if state == MISSING {
        (tr("Needs a one-time download (about {mb} MB).").field("mb", &mb), pack.available)
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

/// The status line under Background's High quality cutout switch (the vision status row's twin for the cutout's own download): (text,
/// whether Download shows). Hidden while the text is empty. `on` is the switch. A cutout that isn't there yet or didn't load never stops the
/// picture: the normal one is used meanwhile, which the text says.
pub fn matting_text(on: bool, state: &str, pack: &VisionPack) -> (String, bool) {
    if !on || state == LOADING || (pack.ready && state != FAILED) {
        return (String::new(), false);
    }
    if pack.downloading {
        let text = if let Some(percent) = (pack.done * 100).checked_div(pack.size) {
            tr("Downloading… {percent}%").field("percent", percent.min(99).to_string())
        } else {
            tr("Downloading…")
        };
        return (text, false);
    }
    if state == FAILED {
        return (tr("The high quality cutout didn't load on this computer, so the normal one is used."), false);
    }
    if !pack.available {
        return (tr("There's no high quality cutout for this computer yet, so the normal one is used."), false);
    }
    if !pack.error.is_empty() {
        return (pack.error.clone(), true);
    }
    (tr("Needs a one-time download (about {mb} MB). Until then the normal cutout is used.").field("mb", pack.size_mb.max(1).to_string()), true)
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
    /// while it can't say. The same frame (by number) asked twice is worked out once. The model runs elsewhere and this hands over
    /// the newest mask it has, about a frame behind; only a stream's first mask is waited for.
    fn person_mask(&self, source_id: &str, frame: &Frame) -> Option<Arc<Vec<u8>>>;
    /// person_mask at w x h instead of the frame's size, made at that size. The default shrinks person_mask's.
    fn person_mask_at(&self, source_id: &str, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        let mask = self.person_mask(source_id, frame)?;
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        if mask.len() != fw * fh {
            return None;
        }
        if (w, h) == (fw, fh) {
            return Some(mask);
        }
        Some(Arc::new(crate::image::resize_area(&mask, fw, fh, 1, w, h)))
    }
    /// How many times the view has turned (0 for a service that doesn't count). A frame's picture is of the view it went through, so a step asking for its mask says which count that was (person_mask_in).
    fn view_epoch(&self) -> u64 {
        0
    }
    /// person_mask_at for a frame whose picture was made under view_epoch() == epoch: None once the view has turned since, as a mask of the old way round isn't the new picture's. The default doesn't know of turns.
    fn person_mask_in(&self, source_id: &str, frame: &Frame, w: usize, h: usize, _epoch: u64) -> Option<Arc<Vec<u8>>> {
        // At the frame's own size it's person_mask as it was, not shrunk through person_mask_at
        if (w, h) == (frame.width as usize, frame.height as usize) {
            return self.person_mask(source_id, frame);
        }
        self.person_mask_at(source_id, frame, w, h)
    }
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
    fn remove_pack(&self) -> Result<(), String>;
    /// What went wrong after the download itself, shown like its own failure on the waiting plugin's tile.
    fn download_failed(&self, _error: &str, _plugin: &str) {}
    /// From a frame step: the same as person_mask_in (at w x h), from the high quality cutout's model (Robust Video Matting, run by ONNX Runtime from its own
    /// download). Never waits for it, and while it isn't downloaded, loading, failed or late this is person_mask's answer. A service without
    /// the model leaves this as it is. epoch is person_mask_in's: a picture of a view that has turned since gets no mask of either model.
    fn person_mask_hq(&self, source_id: &str, frame: &Frame, w: usize, h: usize, epoch: u64) -> Option<Arc<Vec<u8>>> {
        self.person_mask_in(source_id, frame, w, h, epoch)
    }
    /// OFF until something asks for the high quality cutout, then LOADING and READY; MISSING (without loading) when it isn't downloaded; FAILED.
    fn matting_state(&self) -> String {
        OFF.into()
    }
    /// Where the high quality cutout's download is.
    fn matting_pack(&self) -> VisionPack {
        VisionPack::default()
    }
    /// Download it in the background; vision_download_progress and vision_download_finished (with no plugin to switch on) say how it goes.
    fn download_matting(&self) {}
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

    #[test]
    fn the_cutout_note_says_the_normal_one_is_used_while_it_isnt_there() {
        let pack = VisionPack { available: true, size_mb: 26, ..VisionPack::default() };
        assert_eq!(matting_text(false, MISSING, &pack), (String::new(), false));
        assert_eq!(matting_text(true, MISSING, &pack), ("Needs a one-time download (about 26 MB). Until then the normal cutout is used.".into(), true));
        assert!(matting_text(true, OFF, &pack).1);
        assert_eq!(matting_text(true, MISSING, &VisionPack { available: false, ..pack.clone() }).0, "There's no high quality cutout for this computer yet, so the normal one is used.");
        assert_eq!(matting_text(true, MISSING, &VisionPack { error: "The download failed.".into(), ..pack.clone() }), ("The download failed.".into(), true));
        assert_eq!(matting_text(true, MISSING, &VisionPack { downloading: true, done: 1, size: 4, ..pack.clone() }).0, "Downloading… 25%");
        let ready = VisionPack { ready: true, ..pack };
        // Loading, loaded and not yet asked for are all quiet; only a model that won't load says anything
        for state in [OFF, LOADING, READY] {
            assert_eq!(matting_text(true, state, &ready), (String::new(), false), "{state}");
        }
        assert_eq!(matting_text(true, FAILED, &ready).0, "The high quality cutout didn't load on this computer, so the normal one is used.");
    }
}
