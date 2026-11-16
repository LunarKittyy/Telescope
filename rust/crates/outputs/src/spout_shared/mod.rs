//! What every Spout plugin shares, kept apart from any one plugin: Spout2's layout of the sender list, a sender's info and its
//! locks (`protocol`, plain data) and the Windows side that maps, locks and reads them through D3D11 (`win`).

pub mod protocol;
#[cfg(windows)]
pub mod win;
