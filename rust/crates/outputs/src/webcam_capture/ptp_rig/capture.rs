//! Taking a photo to the computer over PTP, the way libgphoto2's drivers do it: Nikon's capture into the camera's memory, Canon's remote
//! release with the picture sent to the computer. Both then wait for the camera to say a file was made and fetch it.
//! Nothing here has met a real camera yet, only the mock (ptp/mock.rs).

use super::*;
use crate::webcam_capture::ptp::{object_name, Cursor, OP_GET_OBJECT, OP_GET_OBJECT_INFO};

const NIKON_GET_EVENT: u16 = 0x90C7;
const NIKON_CAPTURE_TO_MEMORY: u16 = 0x90C0;
/// Nikon says a picture is waiting in its memory (the handle is the event's parameter), and that the capture is over.
const NIKON_EVENT_OBJECT_IN_MEMORY: u16 = 0xC101;
/// The standard event for a file added to the card.
const EVENT_OBJECT_ADDED: u16 = 0x4002;
/// The handle Nikon gives a picture in its memory when the event doesn't name one.
const NIKON_MEMORY_HANDLE: u32 = 0xFFFF_0001;
/// Nikon wants no handle: any storage.
const NIKON_ANY_STORAGE: u32 = 0xFFFF_FFFF;

const CANON_REMOTE_RELEASE_ON: u16 = 0x9128;
const CANON_REMOTE_RELEASE_OFF: u16 = 0x9129;
const CANON_TRANSFER_COMPLETE: u16 = 0x9117;
/// Where Canon puts a photo, and the value that sends it to the computer.
const CANON_CAPTURE_DESTINATION: u16 = 0xD11C;
const CANON_TO_COMPUTER: u32 = 4;
/// The button states of Canon's remote release: half way (focus) and all the way.
const HALF_PRESS: u32 = 1;
const FULL_PRESS: u32 = 2;
/// A folder the camera made, which isn't a photo.
const OFC_ASSOCIATION: u16 = 0x3001;

/// How long a photo may take: libgphoto2 waits 70 seconds, and a long exposure with noise reduction can use most of it.
pub const PHOTO_WAIT: Duration = Duration::from_secs(70);
/// How long the second file of a RAW and JPEG pair may trail the first.
const NEXT_FILE_WAIT: Duration = Duration::from_secs(10);
/// How many busy answers' worth of waiting a half-pressed Canon gets to focus before the full press.
const FOCUS_SETTLE: u32 = 10;

/// A Canon event that says a file was made: its handle and name, None for a folder or a record too short to hold them.
pub fn canon_added(kind: u32, record: &[u8]) -> Option<(u32, String)> {
    // The record's offsets are given from its start, header included; this one starts after the 8 byte header
    let name_at = if kind == CANON_EVENT_OBJECT_ADDED { 0x28 - 8 } else { 0x2c - 8 };
    let handle = read_u32(record, 0)?;
    let format = record.get(8..10).map(|b| u16::from_le_bytes([b[0], b[1]]))?;
    let name = record.get(name_at..)?;
    if format == OFC_ASSOCIATION || name.is_empty() {
        return None;
    }
    let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    Some((handle, String::from_utf8_lossy(&name[..end]).into_owned()))
}

impl PtpCamera {
    /// Takes one photo to the computer and gives its files, `expect` of them (RAW and JPEG are two).
    pub(super) fn take_picture(&mut self, expect: usize) -> Result<Vec<Shot>, Incomplete> {
        match self.maker {
            Maker::Nikon => self.nikon_trigger()?,
            Maker::Canon => self.canon_trigger()?,
            Maker::Other => return Err("photos aren't known for this camera's make".into()),
        }
        let mut shots: Vec<Shot> = Vec::new();
        let mut until = Instant::now() + self.photo_wait;
        while shots.len() < expect.max(1) {
            match self.next_object(until) {
                Ok(Some((handle, name))) => match self.fetch_object(handle, name) {
                    Ok(shot) => {
                        shots.push(shot);
                        until = until.min(Instant::now() + NEXT_FILE_WAIT);
                    }
                    Err(why) => return Err(missing(shots, expect, why)),
                },
                Ok(None) => return Err(missing(shots, expect, "the photo didn't arrive in time".into())),
                Err(why) => return Err(missing(shots, expect, why)),
            }
        }
        Ok(shots)
    }

    /// Starts a photo that goes to the camera's memory, so no card is needed.
    fn nikon_trigger(&mut self) -> Result<(), String> {
        if !self.info.supports(NIKON_CAPTURE_TO_MEMORY) {
            return Err("this camera can't be triggered from the computer".into());
        }
        if self.info.properties.contains(&NIKON_RECORDING_MEDIA) {
            let _ = self.set_raw(NIKON_RECORDING_MEDIA, 1);
        }
        // Whatever the camera said before this photo isn't about it
        self.nikon_events();
        self.nikon_added.clear();
        self.nikon_seen.clear();
        self.busy_run(NIKON_CAPTURE_TO_MEMORY, &[NIKON_ANY_STORAGE])?;
        // The shutter and the processing keep the camera busy until it is ready again
        self.busy_run(NIKON_DEVICE_READY, &[]).map(|_| ())
    }

    /// What Nikon has to report: a count, then each event's code and parameter.
    fn nikon_events(&mut self) -> Vec<(u16, u32)> {
        let Ok(reply) = self.t.transact(NIKON_GET_EVENT, &[], None) else { return Vec::new() };
        if !reply.ok() {
            return Vec::new();
        }
        let mut c = Cursor::new(&reply.data);
        let count = c.u16().unwrap_or(0);
        (0..count).map_while(|_| Some((c.u16()?, c.u32()?))).collect()
    }

    /// Presses Canon's shutter button in two steps, with the picture sent to the computer.
    fn canon_trigger(&mut self) -> Result<(), String> {
        if !self.info.supports(CANON_REMOTE_RELEASE_ON) {
            return Err("this camera can't be triggered from the computer".into());
        }
        if self.canon.contains_key(&CANON_CAPTURE_DESTINATION) {
            let _ = self.canon_set(CANON_CAPTURE_DESTINATION as u32, CANON_TO_COMPUTER);
        }
        self.read_canon_events();
        self.canon_added.clear();
        self.busy_run(CANON_REMOTE_RELEASE_ON, &[HALF_PRESS, 0])?;
        std::thread::sleep(self.pacing.busy_wait * FOCUS_SETTLE);
        let full = self.busy_run(CANON_REMOTE_RELEASE_ON, &[FULL_PRESS, 0]);
        // The buttons come up whether or not the full press worked, or the camera stays half pressed
        let _ = self.t.transact(CANON_REMOTE_RELEASE_OFF, &[FULL_PRESS], None);
        let _ = self.t.transact(CANON_REMOTE_RELEASE_OFF, &[HALF_PRESS], None);
        full.map(|_| ())
    }

    /// The next file the camera made, waiting until `until`: its handle and the name it gave when it said so. None when time ran out.
    fn next_object(&mut self, until: Instant) -> Result<Option<(u32, String)>, String> {
        loop {
            match self.maker {
                Maker::Canon => {
                    if let Some(added) = self.canon_added.pop_front() {
                        return Ok(Some(added));
                    }
                    self.read_canon_events();
                    if let Some(added) = self.canon_added.pop_front() {
                        return Ok(Some(added));
                    }
                }
                _ => {
                    for (code, param) in self.nikon_events() {
                        if matches!(code, NIKON_EVENT_OBJECT_IN_MEMORY | EVENT_OBJECT_ADDED) {
                            let handle = if param == 0 { NIKON_MEMORY_HANDLE } else { param };
                            if !self.nikon_seen.contains(&handle) {
                                self.nikon_seen.push(handle);
                                self.nikon_added.push_back(handle);
                            }
                        }
                    }
                    if let Some(handle) = self.nikon_added.pop_front() {
                        return Ok(Some((handle, String::new())));
                    }
                }
            }
            if Instant::now() >= until {
                return Ok(None);
            }
            std::thread::sleep(self.pacing.busy_wait);
        }
    }

    /// Downloads a file the camera made, named as it said or else as the camera's file information has it.
    fn fetch_object(&mut self, handle: u32, said: String) -> Result<Shot, String> {
        let name = if said.is_empty() {
            let info = ptp::run(self.t.as_mut(), OP_GET_OBJECT_INFO, &[handle], None).ok();
            info.and_then(|r| object_name(&r.data)).unwrap_or_else(|| format!("IMG_{handle:08X}.JPG"))
        } else {
            said
        };
        let data = ptp::run(self.t.as_mut(), OP_GET_OBJECT, &[handle], None)?.data;
        if data.is_empty() {
            return Err("the camera sent an empty file".into());
        }
        if self.maker == Maker::Canon {
            // Tells the camera it can let go of the file
            let _ = self.t.transact(CANON_TRANSFER_COMPLETE, &[handle], None);
        }
        Ok(Shot { name, data })
    }
}

fn missing(shots: Vec<Shot>, expect: usize, why: String) -> Incomplete {
    if shots.is_empty() {
        Incomplete { why, shots }
    } else {
        Incomplete { why: format!("{} of {expect} files came: {why}", shots.len()), shots }
    }
}
