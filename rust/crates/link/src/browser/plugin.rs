//! The Browser camera card and its sources (plugins/browser_camera.py).

use telescope_core::i18n::tr;

/// What the stream says while it waits for a browser (BrowserReader.waiting_text).
pub fn waiting_text() -> String {
    tr("Waiting for the browser. Scan the code on the Browser camera card and tap Start.")
}
