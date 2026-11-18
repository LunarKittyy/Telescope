//! What the DSLR capture shares between the systems that have one: how it waits and gives up, and the error that says another program has the camera.

use std::time::Duration;

/// What gphoto2 says when something else (the file manager, usually) has mounted the camera.
pub const CLAIM_ERROR: &str = "Could not claim the USB device";

pub fn is_claim_error(why: &str) -> bool {
    why.contains(CLAIM_ERROR)
}

/// How the capture waits and gives up.
#[derive(Clone, Copy)]
pub struct Timing {
    /// Before starting gphoto2 again after it ended; doubles while it sends nothing.
    pub restart: Duration,
    pub restart_max: Duration,
    /// Runs in a row that sent nothing before it counts as failed.
    pub give_up_runs: u32,
    /// Without a frame for this long a read fails (and gphoto2 is killed, so the next try can claim the camera).
    pub silent: Duration,
    /// How long gphoto2 gets to turn live view off after an interrupt before it is killed.
    pub grace: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing { restart: Duration::from_millis(200), restart_max: Duration::from_secs(4), give_up_runs: 5, silent: Duration::from_secs(15), grace: Duration::from_secs(3) }
    }
}
