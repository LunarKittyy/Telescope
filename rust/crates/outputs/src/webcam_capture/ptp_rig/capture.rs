//! Taking a photo to the computer over PTP, the way libgphoto2's drivers do it: Nikon's capture into the camera's memory, Canon's remote
//! release with the picture sent to the computer. Both then wait for the camera to say a file was made and fetch it.
//! Nothing here has met a real camera yet, only the mock (ptp/mock.rs).

use super::*;
use crate::webcam_capture::ptp::{Cursor, ObjectInfo, OFC_ASSOCIATION, OP_DELETE_OBJECT, OP_GET_OBJECT, OP_GET_OBJECT_INFO, OP_GET_PARTIAL_OBJECT};

const NIKON_GET_EVENT: u16 = 0x90C7;
const NIKON_CAPTURE_TO_MEMORY: u16 = 0x90C0;
/// Deletes a picture held in the camera's memory, for the cameras that won't take the standard delete for it.
const NIKON_DELETE_MEMORY_IMAGE: u16 = 0x90C3;
/// Nikon says a picture is waiting in its memory (the handle is the event's parameter), and that the capture is over.
const NIKON_EVENT_OBJECT_IN_MEMORY: u16 = 0xC101;
/// The standard event for a file added to the card.
const EVENT_OBJECT_ADDED: u16 = 0x4002;
/// The handle Nikon gives a picture in its memory when the event doesn't name one.
const NIKON_MEMORY_HANDLE: u32 = 0xFFFF_0001;
/// Nikon wants no handle: any storage.
const NIKON_ANY_STORAGE: u32 = 0xFFFF_FFFF;
/// The standard response for a handle the camera doesn't know.
const RC_INVALID_OBJECT_HANDLE: u16 = 0x2009;

const CANON_REMOTE_RELEASE_ON: u16 = 0x9128;
const CANON_REMOTE_RELEASE_OFF: u16 = 0x9129;
const CANON_TRANSFER_COMPLETE: u16 = 0x9117;
/// Where Canon puts a photo, and the value that sends it to the computer.
const CANON_CAPTURE_DESTINATION: u16 = 0xD11C;
const CANON_TO_COMPUTER: u32 = 4;
/// The button states of Canon's remote release: half way (focus), all the way, and the EOS M's single press.
const HALF_PRESS: u32 = 1;
const FULL_PRESS: u32 = 2;
const EOS_M_PRESS: u32 = 3;
/// A file Canon offers to send to the computer, in the older and the newer layout.
const CANON_EVENT_REQUEST_TRANSFER: u32 = 0xC186;
const CANON_EVENT_REQUEST_TRANSFER_64: u32 = 0xC1A9;

/// How long a photo may take: libgphoto2 waits 70 seconds, and a long exposure with noise reduction can use most of it. The camera's
/// readiness and its announcement of the file both count against it.
pub const PHOTO_WAIT: Duration = Duration::from_secs(70);
/// How long the second file of a RAW and JPEG pair may trail the first.
const NEXT_FILE_WAIT: Duration = Duration::from_secs(10);
/// How many busy answers' worth of waiting a half-pressed Canon gets to focus before the full press.
const FOCUS_SETTLE: u32 = 10;
/// How much of a file is asked for at a time: libgphoto2 reads Canon's pictures in pieces of this size, and a transport that caps one
/// transfer (Windows' does) never sees more than this.
const PIECE: u32 = 1 << 20;
/// The largest file taken, which no camera's picture comes near.
const LONGEST_FILE: u32 = 512 << 20;

/// A Canon event that says a file was made, as (handle, name, offered to send): a file added to the card, or one the camera asks to send.
pub fn canon_added(kind: u32, record: &[u8]) -> Option<(u32, String, bool)> {
    // The record's offsets are given from its start, header included; this one starts after the 8 byte header
    let (format_at, name_at, transfer) = match kind {
        super::CANON_EVENT_OBJECT_ADDED => (0x10 - 8, 0x28 - 8, false),
        super::CANON_EVENT_OBJECT_ADDED_64 => (0x10 - 8, 0x2c - 8, false),
        CANON_EVENT_REQUEST_TRANSFER | CANON_EVENT_REQUEST_TRANSFER_64 => (0x0c - 8, 0x1c - 8, true),
        _ => return None,
    };
    let handle = read_u32(record, 0)?;
    let format = record.get(format_at..format_at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))?;
    let name = record.get(name_at..)?;
    if format == OFC_ASSOCIATION || name.is_empty() {
        return None;
    }
    let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    Some((handle, String::from_utf8_lossy(&name[..end]).into_owned(), transfer))
}

/// Whether a model name is one of Canon's EOS M, which takes a single press where the others take two.
pub(super) fn is_eos_m(model: &str) -> bool {
    model.find("EOS M").is_some_and(|at| model[at + 5..].chars().next().is_none_or(|c| c.is_ascii_digit() || c == ' '))
}

fn stopped() -> String {
    "stopped".into()
}

impl PtpCamera {
    /// Takes one photo to the computer and gives its files, `expect` of them (RAW and JPEG are two).
    pub(super) fn take_picture(&mut self, expect: usize) -> Result<Vec<Shot>, Incomplete> {
        let mut until = Instant::now() + self.photo_wait;
        self.nikon_added.clear();
        self.nikon_seen.clear();
        // A live view that wouldn't stop (a busy camera) would leave the shutter unable to fire
        if self.live {
            self.end_live_view();
            if self.live {
                return Err("the camera is busy ending live view".into());
            }
        }
        let triggered = match self.maker {
            Maker::Nikon => self.nikon_trigger(until),
            Maker::Canon => self.canon_trigger(),
            Maker::Other => Err("photos aren't known for this camera's make".into()),
        };
        let mut shots: Vec<Shot> = Vec::new();
        let outcome = triggered.map_err(Incomplete::from).and_then(|()| {
            while shots.len() < expect.max(1) {
                match self.next_object(until) {
                    Ok(Some((handle, name, transfer))) => match self.fetch_object(handle, name, transfer) {
                        Ok(Some(shot)) => {
                            shots.push(shot);
                            until = until.min(Instant::now() + NEXT_FILE_WAIT);
                        }
                        // A folder the camera made, not a photo
                        Ok(None) => {}
                        Err(why) => return Err(missing(std::mem::take(&mut shots), expect, why)),
                    },
                    Ok(None) => return Err(missing(std::mem::take(&mut shots), expect, "the photo didn't arrive in time".into())),
                    Err(why) => return Err(missing(std::mem::take(&mut shots), expect, why)),
                }
            }
            Ok(std::mem::take(&mut shots))
        });
        // Pictures the camera still holds in its memory are let go of, taken or not, or its buffer fills
        self.nikon_release();
        outcome
    }

    /// Starts a photo that goes to the camera's memory, so no card is needed.
    fn nikon_trigger(&mut self, until: Instant) -> Result<(), String> {
        if !self.info.supports(NIKON_CAPTURE_TO_MEMORY) {
            return Err("this camera can't be triggered from the computer".into());
        }
        if self.info.properties.contains(&NIKON_RECORDING_MEDIA) {
            let _ = self.set_raw(NIKON_RECORDING_MEDIA, 1);
        }
        // Whatever the camera said before this photo isn't about it
        self.nikon_events();
        self.busy_run(NIKON_CAPTURE_TO_MEMORY, &[NIKON_ANY_STORAGE])?;
        // The shutter and the processing keep the camera busy until it is ready again, which a long exposure stretches
        self.busy_until(NIKON_DEVICE_READY, &[], until)
    }

    /// Runs an operation until the camera stops saying it is busy, or `until` passes, or the camera is stopped.
    fn busy_until(&mut self, op: u16, params: &[u32], until: Instant) -> Result<(), String> {
        loop {
            let reply = self.t.transact(op, params, None)?;
            if reply.code != ptp::RC_DEVICE_BUSY {
                return if reply.ok() { Ok(()) } else { Err(format!("the camera refused operation {op:#06x} ({:#06x})", reply.code)) };
            }
            if self.is_stopped() {
                return Err(stopped());
            }
            if Instant::now() >= until {
                return Err("the camera stayed busy".into());
            }
            std::thread::sleep(self.pacing.busy_wait);
        }
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

    /// Presses Canon's shutter button, with the picture sent to the computer, and lets go of it whatever happened.
    fn canon_trigger(&mut self) -> Result<(), String> {
        if !self.info.supports(CANON_REMOTE_RELEASE_ON) {
            return Err("this camera can't be triggered from the computer".into());
        }
        if self.canon.contains_key(&CANON_CAPTURE_DESTINATION) {
            let _ = self.canon_set(CANON_CAPTURE_DESTINATION as u32, CANON_TO_COMPUTER);
        }
        self.read_canon_events();
        self.canon_added.clear();
        // The buttons that went down, which are the ones to bring up
        let mut down: Vec<u32> = Vec::new();
        let pressed = if is_eos_m(&self.info.model) {
            // The EOS M takes a single press that focuses and shoots
            self.busy_run(CANON_REMOTE_RELEASE_ON, &[EOS_M_PRESS, 0]).map(|_| down.push(EOS_M_PRESS))
        } else {
            self.busy_run(CANON_REMOTE_RELEASE_ON, &[HALF_PRESS, 0]).map(|_| down.push(HALF_PRESS)).and_then(|()| {
                std::thread::sleep(self.pacing.busy_wait * FOCUS_SETTLE);
                self.busy_run(CANON_REMOTE_RELEASE_ON, &[FULL_PRESS, 0]).map(|_| down.push(FULL_PRESS))
            })
        };
        let released = self.canon_release(&down);
        pressed.map(|_| ()).and(released)
    }

    /// Brings the buttons up, the last pressed first, checking each answer. A camera that won't let go is reset by leaving and entering
    /// remote mode, which releases whatever is held.
    fn canon_release(&mut self, down: &[u32]) -> Result<(), String> {
        let mut failed = false;
        for button in down.iter().rev() {
            self.read_canon_events();
            if self.busy_run(CANON_REMOTE_RELEASE_OFF, &[*button]).is_err() {
                failed = true;
            }
        }
        if !failed {
            return Ok(());
        }
        let _ = self.t.transact(super::CANON_SET_REMOTE_MODE, &[0], None);
        let _ = self.t.transact(super::CANON_SET_REMOTE_MODE, &[1], None);
        Err("the camera didn't let go of the shutter button, so it was reset".into())
    }

    /// The next file the camera made, waiting until `until`: its handle, the name it gave when it said so, and whether it offered to send it.
    /// None when time ran out.
    fn next_object(&mut self, until: Instant) -> Result<Option<(u32, String, bool)>, String> {
        loop {
            if self.is_stopped() {
                return Err(stopped());
            }
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
                        return Ok(Some((handle, String::new(), false)));
                    }
                }
            }
            if Instant::now() >= until {
                return Ok(None);
            }
            std::thread::sleep(self.pacing.busy_wait);
        }
    }

    /// Downloads a file the camera made, named as it said or else as the camera's file information has it, in pieces so a transport that
    /// limits one transfer can still carry a RAW file. None for a folder. `transfer` is a file Canon offered to send, which it is told is
    /// done with.
    fn fetch_object(&mut self, handle: u32, said: String, transfer: bool) -> Result<Option<Shot>, String> {
        let info = ptp::run(self.t.as_mut(), OP_GET_OBJECT_INFO, &[handle], None).ok().and_then(|r| ObjectInfo::parse(&r.data));
        if info.as_ref().is_some_and(|i| i.format == OFC_ASSOCIATION) {
            return Ok(None);
        }
        let name = if said.is_empty() { info.as_ref().and_then(|i| i.name.clone()).unwrap_or_else(|| format!("IMG_{handle:08X}.JPG")) } else { said };
        let size = info.as_ref().map_or(0, |i| i.size);
        let data = if size > 0 && self.info.supports(OP_GET_PARTIAL_OBJECT) { self.fetch_pieces(handle, size)? } else { self.fetch_whole(handle)? };
        if data.is_empty() {
            return Err("the camera sent an empty file".into());
        }
        if transfer {
            // Tells the camera it can let go of the file
            let _ = self.t.transact(CANON_TRANSFER_COMPLETE, &[handle], None);
        }
        Ok(Some(Shot { name, data }))
    }

    fn fetch_whole(&mut self, handle: u32) -> Result<Vec<u8>, String> {
        Ok(ptp::run(self.t.as_mut(), OP_GET_OBJECT, &[handle], None)?.data)
    }

    /// A file of `size` bytes in pieces, checked to come out the length the camera said.
    fn fetch_pieces(&mut self, handle: u32, size: u32) -> Result<Vec<u8>, String> {
        if size > LONGEST_FILE {
            return Err(format!("the camera's file is {size} bytes, which is too large"));
        }
        let mut data = Vec::with_capacity(size as usize);
        while (data.len() as u32) < size {
            if self.is_stopped() {
                return Err(stopped());
            }
            let want = (size - data.len() as u32).min(PIECE);
            let piece = ptp::run(self.t.as_mut(), OP_GET_PARTIAL_OBJECT, &[handle, data.len() as u32, want], None)?.data;
            if piece.is_empty() || piece.len() > want as usize {
                return Err(format!("the camera sent a piece of {} bytes when {want} were asked for", piece.len()));
            }
            data.extend_from_slice(&piece);
        }
        Ok(data)
    }

    /// Lets go of the pictures a Nikon camera holds in its memory for this photo: the standard delete, and Nikon's own for a camera that
    /// refuses it. A camera that says it doesn't know the handle isn't asked again.
    fn nikon_release(&mut self) {
        let handles = std::mem::take(&mut self.nikon_seen);
        self.nikon_added.clear();
        if self.maker != Maker::Nikon || self.nikon_delete_fails {
            return;
        }
        for handle in handles {
            let Ok(reply) = self.t.transact(OP_DELETE_OBJECT, &[handle, 0], None) else { return };
            if reply.ok() {
                continue;
            }
            if reply.code == RC_INVALID_OBJECT_HANDLE {
                self.nikon_delete_fails = true;
                return;
            }
            let _ = self.t.transact(NIKON_DELETE_MEMORY_IMAGE, &[handle], None);
        }
    }
}

fn missing(shots: Vec<Shot>, expect: usize, why: String) -> Incomplete {
    if shots.is_empty() {
        Incomplete { why, shots }
    } else {
        Incomplete { why: format!("{} of {expect} files came: {why}", shots.len()), shots }
    }
}
