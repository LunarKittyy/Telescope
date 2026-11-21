//! How often the plugins that look for cameras and senders look: every couple of seconds while someone can see the window, and slowly
//! while it sits in the tray with nothing streaming and nobody watching the camera.

use telescope_core::event::Event;

/// The look while nobody is around; a camera plugged in meanwhile shows up within this, or at once when the window or a viewer arrives.
pub const HIDDEN_SCAN_MS: u64 = 30_000;

#[derive(Default)]
pub struct ScanPace {
    hidden: bool,
    watched: bool,
    slow: bool,
}

impl ScanPace {
    fn idle(&self, streaming: bool) -> bool {
        self.hidden && !self.watched && !streaming
    }

    /// When the next look is due after one, given the usual wait.
    pub fn next_ms(&mut self, usual: u64, streaming: bool) -> u64 {
        self.slow = self.idle(streaming);
        if self.slow { HIDDEN_SCAN_MS.max(usual) } else { usual }
    }

    /// Takes in what may change the pace; true when a look that was waiting out the slow wait should happen now.
    pub fn on_event(&mut self, event: &Event, streaming: bool) -> bool {
        match event {
            Event::WindowShown { shown } => self.hidden = !shown,
            Event::CameraWatched { watched } => self.watched = *watched,
            Event::StreamsChanged { .. } => {}
            _ => return false,
        }
        let hurry = self.slow && !self.idle(streaming);
        if hurry {
            self.slow = false;
        }
        hurry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_idle_app_looks_slowly_and_hurries_when_anyone_arrives() {
        let mut pace = ScanPace::default();
        assert_eq!(pace.next_ms(2000, false), 2000, "shown until told otherwise");
        assert!(!pace.on_event(&Event::WindowShown { shown: false }, false), "nothing was waiting yet");
        assert_eq!(pace.next_ms(2000, false), HIDDEN_SCAN_MS);
        assert_eq!(pace.next_ms(2000, true), 2000, "a stream needs unplugging noticed");
        assert_eq!(pace.next_ms(2000, false), HIDDEN_SCAN_MS);
        assert!(pace.on_event(&Event::StreamsChanged { count: 1 }, true));
        assert_eq!(pace.next_ms(2000, true), 2000);
        assert_eq!(pace.next_ms(2000, false), HIDDEN_SCAN_MS);
        assert!(pace.on_event(&Event::CameraWatched { watched: true }, false));
        assert_eq!(pace.next_ms(2000, false), 2000);
        pace.on_event(&Event::CameraWatched { watched: false }, false);
        assert_eq!(pace.next_ms(2000, false), HIDDEN_SCAN_MS);
        assert!(pace.on_event(&Event::WindowShown { shown: true }, false));
    }
}
