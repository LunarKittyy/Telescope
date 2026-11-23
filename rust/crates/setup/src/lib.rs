//! Setup (Advanced settings: the virtual camera, the canvas, video and microphone limits, the look, the phone app and
//! About), Adjustments (what Telescope changes by itself, said and undoable), Monitoring (the Phone card: battery and temperature with their warnings) and Stats (the card with the numbers of the stream).

pub mod adjustments;
pub mod monitoring;
pub mod py;
pub mod setup;
pub mod stats;
pub mod stats_phone;
pub(crate) mod titles;
