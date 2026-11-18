//! What every Spout plugin shares, kept apart from any one plugin: Spout2's layout of the sender list, a sender's info and its
//! locks (`protocol`, plain data), the plane arithmetic of the pictures read back from the card (`planes`) and the Windows side that maps,
//! locks, converts and reads them through D3D11 (`win`).

pub mod planes;
pub mod protocol;
#[cfg(windows)]
pub mod win;
