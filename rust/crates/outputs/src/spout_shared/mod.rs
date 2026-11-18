//! What every Spout plugin shares, kept apart from any one plugin: Spout2's layout of the sender list, a sender's info and its
//! locks (`protocol`, plain data), the plane arithmetic of the pictures read back from the card (`planes`), the waiting for a readback
//! that can be stopped (`poll`) and the Windows side that maps, locks, converts and reads them through D3D11 (`win`).

pub mod planes;
#[cfg(any(windows, test))]
pub mod poll;
pub mod protocol;
#[cfg(windows)]
pub mod win;
