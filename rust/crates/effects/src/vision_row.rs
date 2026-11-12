//! The status row on cards that use vision (widgets/vision_status.py), for ui/vision_status.slint: why nothing's happening yet, with a Download button when that's why.

use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::vision::{matting_text, status_text};

/// The row's properties. active: the card is switched to something that needs vision; problem: the card's own reason, shown first.
pub fn props(host: &dyn Host, active: bool, problem: &str) -> Value {
    props_for(host, active, problem, false)
}

/// The same for a card that works without vision too, only better with it (optional).
pub fn props_for(host: &dyn Host, active: bool, problem: &str, optional: bool) -> Value {
    let (text, download) = status_text(active, problem, optional, &host.vision_state(), &host.vision_pack());
    json!({"vision-text": text, "vision-download": download})
}

/// The note under Background's High quality cutout switch: why the cutout isn't used yet, with a Download button when that's why.
pub fn cutout_props(host: &dyn Host, on: bool) -> Value {
    let (text, download) = matting_text(on, &host.matting_state(), &host.matting_pack());
    json!({"cutout-text": text, "cutout-download": download})
}

/// Whether this event can change what the cutout's note says.
pub fn changes_cutout(event: &Event) -> bool {
    matches!(event, Event::MattingStateChanged { .. } | Event::VisionDownloadProgress { .. } | Event::VisionDownloadFinished { .. })
}

/// Whether this event can change what the row says.
pub fn changes_it(event: &Event) -> bool {
    matches!(event, Event::VisionStateChanged { .. } | Event::VisionDownloadProgress { .. } | Event::VisionDownloadFinished { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;
    use telescope_core::vision::{VisionPack, LOADING, MISSING};

    #[test]
    fn the_row_follows_the_host() {
        let mut host = FakeHost::new();
        host.vision = Some(LOADING.into());
        assert_eq!(props(&host, true, ""), json!({"vision-text": "Getting ready…", "vision-download": false}));
        assert_eq!(props(&host, false, ""), json!({"vision-text": "", "vision-download": false}));
        host.vision = Some(MISSING.into());
        host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
        assert_eq!(props(&host, true, ""), json!({"vision-text": "Needs a one-time download (about 47 MB).", "vision-download": true}));
        assert!(changes_it(&Event::VisionDownloadProgress { done: 1, size: 2 }) && !changes_it(&Event::StreamStopped));
    }

    #[test]
    fn the_cutout_note_follows_the_host() {
        let mut host = FakeHost::new();
        host.matting = Some(MISSING.into());
        host.matting_pack = VisionPack { available: true, size_mb: 26, ..VisionPack::default() };
        assert_eq!(cutout_props(&host, true), json!({"cutout-text": "Needs a one-time download (about 26 MB). Until then the normal cutout is used.", "cutout-download": true}));
        assert_eq!(cutout_props(&host, false), json!({"cutout-text": "", "cutout-download": false}));
        host.matting_pack.ready = true;
        host.matting = Some("ready".into());
        assert_eq!(cutout_props(&host, true), json!({"cutout-text": "", "cutout-download": false}));
        assert!(changes_cutout(&Event::MattingStateChanged { state: "failed".into() }) && !changes_cutout(&Event::StreamStopped));
    }
}
