//! What the bottom bar can show: readouts a plugin offers and the window pins, in the plain data both sides share.
//!
//! A plugin offers the readouts it can measure with `HostUi::offer_bar_stats` and shows a star for each. The window owns what is pinned,
//! on which side and in what order, because the bar is where the person drags them; plugins ask for a pin with `pin_to_bar` and hear
//! `Event::BarPinsChanged` when the pins change from anywhere. The window's own readouts (the camera, the frame rate and the
//! throughput) are pins like the rest, with the ids below.

use serde::{Deserialize, Serialize};

/// The virtual camera the shown stream goes to, as plain text.
pub const CAMERA: &str = "camera";
/// The shown stream's frame rate.
pub const FPS: &str = "fps";
/// How much data per second reaches this computer from the shown stream.
pub const THROUGHPUT: &str = "throughput";

/// The pins a fresh settings file starts with: (left side, right side), each in the order it is drawn from the left.
pub const DEFAULT_LEFT: &[&str] = &[CAMERA];
pub const DEFAULT_RIGHT: &[&str] = &[FPS, THROUGHPUT];

/// One readout for the bar. Texts are already translated.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BarStat {
    /// Stable across runs, since it is what the settings keep ("<plugin>.<what>").
    pub id: String,
    /// The full name, the tooltip's title.
    pub name: String,
    /// What the tooltip adds under the name, or empty.
    #[serde(default)]
    pub tip: String,
    /// An icon by name (`Icons.named`), shown instead of the word. Empty for a text readout like the camera's.
    #[serde(default)]
    pub icon: String,
    /// The reading, as it is shown. Empty means there is nothing to show now: the pin keeps its place and takes no room.
    pub value: String,
    /// A dim text after the value (the camera's size).
    #[serde(default)]
    pub sub: String,
    /// "" for ink, "warn" or "err" to colour the value.
    #[serde(default)]
    pub tone: String,
}

impl BarStat {
    pub fn new(id: &str, name: &str, icon: &str, value: &str) -> BarStat {
        BarStat { id: id.into(), name: name.into(), icon: icon.into(), value: value.into(), ..BarStat::default() }
    }
}

/// Which side of the bar a pin is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

/// Whether these two lists are the pins a fresh settings file starts with.
pub fn is_default(left: &[String], right: &[String]) -> bool {
    left.iter().map(String::as_str).eq(DEFAULT_LEFT.iter().copied()) && right.iter().map(String::as_str).eq(DEFAULT_RIGHT.iter().copied())
}
