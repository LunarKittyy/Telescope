//! A DSLR over PTP as a `Rig` (gphoto_lib.rs): live view, autofocus, focus steps and the aperture, exposure compensation and white balance
//! menus, with Nikon's and Canon's own operation codes. It words each setting the way libgphoto2 does ("viewfinder", "manualfocusdrive",
//! "f-number" and so on), so the capture thread, the controls and the quirks table that serve cameras on Linux serve these unchanged.
//!
//! The operation codes and value formats are the ones libgphoto2's PTP driver uses for each maker. Nothing here has met a real camera yet,
//! only the mock in the tests (ptp/mock.rs), so each maker's part is listed under "needs a real device" in the lane notes.

use super::gphoto_lib::{Incomplete, Kind, Rig, Setting, Shot};
use super::ptp::{self, find_jpeg, DeviceInfo, Form, Maker, PropDesc, Transport, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Nikon
const NIKON_AF_DRIVE: u16 = 0x90C1;
const NIKON_DEVICE_READY: u16 = 0x90C8;
const NIKON_START_LIVE_VIEW: u16 = 0x9201;
const NIKON_END_LIVE_VIEW: u16 = 0x9202;
const NIKON_GET_LIVE_VIEW_IMG: u16 = 0x9203;
const NIKON_MF_DRIVE: u16 = 0x9204;
/// Where live view frames are kept (1: the camera's memory, so no card is needed).
const NIKON_RECORDING_MEDIA: u16 = 0xD10B;
/// The camera says live view isn't running (it ends by itself every few minutes).
const NIKON_NOT_LIVE_VIEW: u16 = 0xA00B;

// Canon EOS
const CANON_SET_PROP_EX: u16 = 0x9110;
const CANON_SET_REMOTE_MODE: u16 = 0x9114;
const CANON_SET_EVENT_MODE: u16 = 0x9115;
const CANON_GET_EVENT: u16 = 0x9116;
const CANON_GET_VIEW_FINDER_DATA: u16 = 0x9153;
const CANON_DO_AF: u16 = 0x9154;
const CANON_DRIVE_LENS: u16 = 0x9155;
/// Canon's answer while live view isn't ready yet, which waits like a busy camera.
const CANON_NOT_READY: u16 = 0xA102;
const CANON_EVF_OUTPUT_DEVICE: u32 = 0xD1B0;
const CANON_EVF_MODE: u32 = 0xD1B3;
const CANON_APERTURE: u16 = 0xD101;
const CANON_EXPOSURE_COMP: u16 = 0xD104;
const CANON_WHITE_BALANCE: u16 = 0xD10B;
/// What GetEvent's records are: a property's new value, and the values it can have now.
const CANON_EVENT_PROP_VALUE: u32 = 0xC189;
const CANON_EVENT_AVAIL_LIST: u32 = 0xC18A;
/// A file the camera made, in the older and the newer layout.
const CANON_EVENT_OBJECT_ADDED: u32 = 0xC181;
const CANON_EVENT_OBJECT_ADDED_64: u32 = 0xC1A7;
/// How much of a live view frame to ask for.
const CANON_FRAME_SIZE: u32 = 0x0010_0000;
/// How often Canon cameras want their events read, or they stop answering.
const CANON_EVENT_EVERY: Duration = Duration::from_millis(800);

// Properties every maker has, named by the standard
const STD_WHITE_BALANCE: u16 = 0x5005;
/// JPEG or RAW, and how much compression, which is what Nikon's image quality is.
const STD_COMPRESSION: u16 = 0x5004;
const STD_F_NUMBER: u16 = 0x5007;
const STD_EXPOSURE_BIAS: u16 = 0x5010;

/// What a focus step of Canon's lens drive is: libgphoto2's "Near 1" to "Far 3".
const CANON_DRIVE_CHOICES: [(&str, u32); 6] = [("Near 1", 0x0001), ("Near 2", 0x0002), ("Near 3", 0x0003), ("Far 1", 0x8001), ("Far 2", 0x8002), ("Far 3", 0x8003)];

/// How long to wait for a camera that says it is busy.
#[derive(Clone, Copy)]
pub struct Pacing {
    pub busy_wait: Duration,
    pub busy_tries: u32,
}

impl Default for Pacing {
    fn default() -> Pacing {
        Pacing { busy_wait: Duration::from_millis(30), busy_tries: 100 }
    }
}

/// What a Canon camera last said about a property.
#[derive(Default)]
struct CanonProp {
    value: Option<i64>,
    available: Vec<i64>,
}

pub struct PtpCamera {
    t: Box<dyn Transport>,
    info: DeviceInfo,
    maker: Maker,
    pacing: Pacing,
    live: bool,
    canon: HashMap<u16, CanonProp>,
    canon_events: Instant,
    /// Photos a Canon camera said it made (handle and name) that haven't been fetched yet.
    canon_added: VecDeque<(u32, String, bool)>,
    /// Photos a Nikon camera said are waiting (their handles), and the handles already taken from it for this photo.
    nikon_added: VecDeque<u32>,
    nikon_seen: Vec<u32>,
    /// A camera that doesn't know the handles of its memory pictures when asked to delete them isn't asked again.
    nikon_delete_fails: bool,
    /// Set when the camera's thread is asked to end.
    stop: Option<Arc<AtomicBool>>,
    /// How long a photo may take to arrive, an exposure and the camera's processing included.
    photo_wait: Duration,
    closed: bool,
}

impl PtpCamera {
    /// Opens a session on the camera at the other end of `t` and finds out what it is.
    pub fn open(t: Box<dyn Transport>, pacing: Pacing) -> Result<PtpCamera, String> {
        let mut t = t;
        let reply = t.transact(ptp::OP_OPEN_SESSION, &[1], None)?;
        if !reply.ok() && reply.code != ptp::RC_SESSION_ALREADY_OPEN {
            return Err(format!("the camera wouldn't open a session ({:#06x})", reply.code));
        }
        let info = DeviceInfo::parse(&ptp::run(t.as_mut(), ptp::OP_GET_DEVICE_INFO, &[], None)?.data).ok_or("the camera's description didn't make sense")?;
        let maker = info.maker();
        let mut camera = PtpCamera { t, info, maker, pacing, live: false, canon: HashMap::new(), canon_events: Instant::now(), canon_added: VecDeque::new(), nikon_added: VecDeque::new(), nikon_seen: Vec::new(), nikon_delete_fails: false, stop: None, photo_wait: capture::PHOTO_WAIT, closed: false };
        if maker == Maker::Canon {
            // Remote mode lets the computer drive the camera, and the event mode is how it reports back
            ptp::run(camera.t.as_mut(), CANON_SET_REMOTE_MODE, &[1], None)?;
            ptp::run(camera.t.as_mut(), CANON_SET_EVENT_MODE, &[1], None)?;
            camera.read_canon_events();
        }
        Ok(camera)
    }

    pub fn model(&self) -> &str {
        &self.info.model
    }

    pub fn maker(&self) -> Maker {
        self.maker
    }

    /// Runs an operation, waiting while the camera says it is busy.
    fn busy_run(&mut self, op: u16, params: &[u32]) -> Result<ptp::Reply, String> {
        for _ in 0..self.pacing.busy_tries.max(1) {
            let reply = self.t.transact(op, params, None)?;
            if reply.code != ptp::RC_DEVICE_BUSY {
                return if reply.ok() { Ok(reply) } else { Err(format!("the camera refused operation {op:#06x} ({:#06x})", reply.code)) };
            }
            std::thread::sleep(self.pacing.busy_wait);
        }
        Err("the camera stayed busy".into())
    }

    fn start_live_view(&mut self) -> Result<(), String> {
        match self.maker {
            Maker::Nikon => {
                if self.info.properties.contains(&NIKON_RECORDING_MEDIA) {
                    // Frames go to the camera's memory; a camera that won't take it can still show live view
                    let _ = self.set_raw(NIKON_RECORDING_MEDIA, 1);
                }
                self.busy_run(NIKON_START_LIVE_VIEW, &[])?;
            }
            Maker::Canon => {
                // Live view to the computer: the output device is the PC (2), then the mode on
                self.canon_set(CANON_EVF_OUTPUT_DEVICE, 2)?;
                self.canon_set(CANON_EVF_MODE, 1)?;
            }
            Maker::Other => return Err("live view isn't known for this camera's make".into()),
        }
        self.live = true;
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
    }

    /// Live view off. A camera that says it is busy is still showing it, so that stays what the rig believes.
    fn end_live_view(&mut self) {
        let ended = match self.maker {
            Maker::Nikon => self.t.transact(NIKON_END_LIVE_VIEW, &[], None).map(|r| r.code != ptp::RC_DEVICE_BUSY),
            Maker::Canon => self.canon_set(CANON_EVF_OUTPUT_DEVICE, 0).map(|()| true),
            Maker::Other => Ok(true),
        };
        // A camera that can't be reached has nothing to stop either
        if ended.unwrap_or(true) {
            self.live = false;
        }
    }

    /// Sets a Canon property: its code and a 32 bit value, in the record these cameras take.
    fn canon_set(&mut self, prop: u32, value: u32) -> Result<(), String> {
        let mut data = 12u32.to_le_bytes().to_vec();
        data.extend_from_slice(&prop.to_le_bytes());
        data.extend_from_slice(&value.to_le_bytes());
        ptp::run(self.t.as_mut(), CANON_SET_PROP_EX, &[], Some(&data)).map(|_| ())
    }

    /// Reads what a Canon camera has to say (its properties changing among it) and keeps what it said.
    fn read_canon_events(&mut self) {
        self.canon_events = Instant::now();
        let Ok(reply) = self.t.transact(CANON_GET_EVENT, &[], None) else { return };
        if !reply.ok() {
            return;
        }
        let data = &reply.data;
        let mut at = 0;
        while let (Some(size), Some(kind)) = (read_u32(data, at), read_u32(data, at + 4)) {
            let size = size as usize;
            if size < 8 || at + size > data.len() {
                break;
            }
            let record = &data[at + 8..at + size];
            match kind {
                CANON_EVENT_PROP_VALUE => {
                    if let (Some(prop), Some(value)) = (read_u32(record, 0), read_u32(record, 4)) {
                        self.canon.entry(prop as u16).or_default().value = Some(canon_int(prop as u16, value));
                    }
                }
                CANON_EVENT_AVAIL_LIST => {
                    let prop = read_u32(record, 0).map(|p| p as u16);
                    let (format, count) = (read_u32(record, 4), read_u32(record, 8));
                    // Format 3 is a list with a four byte slot per value; a list that doesn't hold what it counts is left out whole
                    if let (Some(prop), Some(3), Some(count)) = (prop, format, count) {
                        let slots = record.get(12..).unwrap_or_default();
                        if count as usize <= slots.len() / 4 {
                            let values = (0..count as usize).filter_map(|i| read_u32(slots, i * 4)).map(|v| canon_int(prop, v)).collect();
                            self.canon.entry(prop).or_default().available = values;
                        }
                    }
                }
                CANON_EVENT_OBJECT_ADDED | CANON_EVENT_OBJECT_ADDED_64 | 0xC186 | 0xC1A9 => {
                    if let Some(added) = capture::canon_added(kind, record) {
                        self.canon_added.push_back(added);
                    }
                }
                _ => {}
            }
            at += size;
        }
    }

    /// The description of a property of the standard kind (read from the camera).
    fn desc(&mut self, code: u16) -> Option<PropDesc> {
        let reply = self.t.transact(ptp::OP_GET_DEVICE_PROP_DESC, &[code as u32], None).ok().filter(ptp::Reply::ok)?;
        PropDesc::parse(&reply.data)
    }

    fn set_raw(&mut self, code: u16, value: i64) -> Result<(), String> {
        let desc = self.desc(code).ok_or("the camera doesn't have that setting")?;
        if !desc.writable {
            return Err("the camera doesn't allow it now".into());
        }
        let bytes = ptp::encode_value(desc.kind, &Value::Int(value)).ok_or("that setting has a kind of value this can't write")?;
        ptp::run(self.t.as_mut(), ptp::OP_SET_DEVICE_PROP_VALUE, &[code as u32], Some(&bytes)).map(|_| ())
    }

    /// Which property a name (libgphoto2's) stands for on this camera, and how its values are worded.
    fn menu(&self, name: &str) -> Option<(u16, Words)> {
        let words = match name {
            "f-number" if self.maker != Maker::Canon => Words::Aperture(self.maker),
            "aperture" if self.maker == Maker::Canon => Words::Aperture(self.maker),
            "exposurecompensation" => Words::Compensation(self.maker),
            "whitebalance" => Words::WhiteBalance(self.maker),
            // Canon's quality is a pair of codes that this doesn't read, so its photos are made as the camera is set
            "imagequality" if self.maker == Maker::Nikon => Words::Quality,
            _ => return None,
        };
        let code = match (name, self.maker) {
            ("aperture", _) => CANON_APERTURE,
            ("exposurecompensation", Maker::Canon) => CANON_EXPOSURE_COMP,
            ("whitebalance", Maker::Canon) => CANON_WHITE_BALANCE,
            ("imagequality", _) => STD_COMPRESSION,
            ("f-number", _) => STD_F_NUMBER,
            ("exposurecompensation", _) => STD_EXPOSURE_BIAS,
            _ => STD_WHITE_BALANCE,
        };
        Some((code, words))
    }

    /// The setting of a menu as the camera lists it now: the raw values on offer and the one in force.
    fn menu_values(&mut self, code: u16) -> Option<(bool, Vec<i64>, Option<i64>)> {
        if self.maker == Maker::Canon {
            if self.canon_events.elapsed() >= CANON_EVENT_EVERY {
                self.read_canon_events();
            }
            let prop = self.canon.get(&code)?;
            return Some((true, prop.available.clone(), prop.value));
        }
        let desc = self.desc(code)?;
        let list = match &desc.form {
            Form::Enum(values) => values.iter().filter_map(Value::int).collect(),
            Form::Range { min, max, step } if *step > 0 => std::iter::successors(Some(*min), |v| v.checked_add(*step)).take_while(|v| v <= max).take(400).collect(),
            _ => Vec::new(),
        };
        Some((desc.writable, list, desc.current.int()))
    }

    fn frame(&mut self) -> Result<Vec<u8>, String> {
        let (mut restarted, mut waited) = (false, 0);
        loop {
            if !self.live {
                self.start_live_view()?;
            }
            let reply = match self.maker {
                Maker::Nikon => self.t.transact(NIKON_GET_LIVE_VIEW_IMG, &[], None)?,
                _ => {
                    if self.canon_events.elapsed() >= CANON_EVENT_EVERY {
                        self.read_canon_events();
                    }
                    self.t.transact(CANON_GET_VIEW_FINDER_DATA, &[CANON_FRAME_SIZE], None)?
                }
            };
            match reply.code {
                ptp::RC_OK => return find_jpeg(&reply.data).map(<[u8]>::to_vec).ok_or_else(|| "the camera sent a frame with no picture in it".to_string()),
                // The first frame after starting isn't ready for a moment
                ptp::RC_DEVICE_BUSY | CANON_NOT_READY if waited < self.pacing.busy_tries => {
                    waited += 1;
                    std::thread::sleep(self.pacing.busy_wait);
                }
                NIKON_NOT_LIVE_VIEW if !restarted => {
                    restarted = true;
                    self.live = false;
                }
                code => return Err(format!("live view stopped ({code:#06x})")),
            }
        }
    }
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A Canon value as the number it stands for: exposure compensation is a signed byte, the rest are plain.
fn canon_int(prop: u16, value: u32) -> i64 {
    if prop == CANON_EXPOSURE_COMP {
        value as u8 as i8 as i64
    } else {
        value as i64
    }
}

/// How a menu's raw values are worded.
#[derive(Clone, Copy)]
enum Words {
    Aperture(Maker),
    Compensation(Maker),
    WhiteBalance(Maker),
    Quality,
}

impl Words {
    fn label(self, raw: i64) -> String {
        match self {
            // Canon counts the aperture in sixteenths of a stop of f-number from f/1 at 8: 0x28 is f/4, 0x30 f/5.6
            Words::Aperture(Maker::Canon) => aperture_label(2f64.powf((raw - 8) as f64 / 16.0)),
            Words::Aperture(_) => aperture_label(raw as f64 / 100.0),
            Words::Compensation(Maker::Canon) => stops_label(canon_stops(raw)),
            Words::Compensation(_) => stops_label(raw as f64 / 1000.0),
            Words::WhiteBalance(maker) => white_balance_label(maker, raw),
            Words::Quality => quality_label(raw),
        }
    }
}

/// The f-numbers lenses are marked with, which a value is worded as the nearest of (the camera's own numbers are 2^(n/6) or so).
const STOPS: [f64; 33] = [
    1.0, 1.1, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.5, 2.8, 3.2, 3.5, 4.0, 4.5, 5.0, 5.6, 6.3, 7.1, 8.0, 9.0, 10.0, 11.0, 13.0, 14.0, 16.0, 18.0, 20.0, 22.0, 25.0, 29.0, 32.0, 36.0, 40.0,
];

fn aperture_label(f: f64) -> String {
    let snapped = STOPS.iter().copied().min_by(|a, b| (a.ln() - f.ln()).abs().total_cmp(&(b.ln() - f.ln()).abs())).filter(|s| (s.ln() - f.ln()).abs() < 0.1).unwrap_or(f);
    number(snapped)
}

/// Exposure compensation as libgphoto2 words it: "0", "-0.7", "1.3".
fn stops_label(ev: f64) -> String {
    number(ev)
}

fn number(n: f64) -> String {
    let r = (n * 10.0).round() / 10.0;
    if (r - r.round()).abs() < 0.001 { format!("{}", r.round() as i64) } else { format!("{r}") }
}

/// A Canon exposure compensation byte in stops: eighths, where the thirds and the half are written as 3, 4 and 5 eighths past a whole.
fn canon_stops(code: i64) -> f64 {
    let (whole, rest) = (code.div_euclid(8), code.rem_euclid(8));
    let part = match rest {
        3 => 1.0 / 3.0,
        4 => 0.5,
        5 => 2.0 / 3.0,
        n => n as f64 / 8.0,
    };
    whole as f64 + part
}

/// Nikon's image quality as libgphoto2 words it for the bodies without a table of their own (the D5100 among them).
fn quality_label(raw: i64) -> String {
    match raw {
        0 => "JPEG Basic",
        1 => "JPEG Normal",
        2 => "JPEG Fine",
        4 => "NEF (Raw)",
        5 => "NEF+Basic",
        6 => "NEF+Normal",
        7 => "NEF+Fine",
        _ => return format!("Setting {raw}"),
    }
    .to_string()
}

fn white_balance_label(maker: Maker, raw: i64) -> String {
    let named = match (maker, raw) {
        (Maker::Canon, 0) => "Auto",
        (Maker::Canon, 1) => "Daylight",
        (Maker::Canon, 2) => "Cloudy",
        (Maker::Canon, 3) => "Tungsten",
        (Maker::Canon, 4) => "Fluorescent",
        (Maker::Canon, 5) => "Flash",
        (Maker::Canon, 6) => "Manual",
        (Maker::Canon, 8) => "Shade",
        (Maker::Canon, 9) => "Color Temperature",
        (_, 1) => "Manual",
        (_, 2) => "Automatic",
        (_, 3) => "One-push Automatic",
        (_, 4) => "Daylight",
        (_, 5) => "Fluorescent",
        (_, 6) => "Tungsten",
        (_, 7) => "Flash",
        (_, 0x8010) => "Cloudy",
        (_, 0x8011) => "Shade",
        (_, 0x8012) => "Color Temperature",
        (_, 0x8013) => "Preset",
        _ => return format!("Setting {raw}"),
    };
    named.to_string()
}

impl Rig for PtpCamera {
    fn preview(&mut self) -> Result<Vec<u8>, String> {
        self.frame()
    }

    fn read(&mut self, name: &str) -> Option<Setting> {
        let setting = |kind, value: String, choices: Vec<String>, readonly| Setting { kind, readonly, value, choices };
        match name {
            "viewfinder" => Some(setting(Kind::Toggle, self.live.to_string(), Vec::new(), false)),
            "autofocusdrive" => {
                let op = if self.maker == Maker::Canon { CANON_DO_AF } else { NIKON_AF_DRIVE };
                (matches!(self.maker, Maker::Nikon | Maker::Canon) && self.info.supports(op)).then(|| setting(Kind::Toggle, "false".into(), Vec::new(), false))
            }
            "manualfocusdrive" => match self.maker {
                Maker::Nikon if self.info.supports(NIKON_MF_DRIVE) => Some(setting(Kind::Range { min: -32767.0, max: 32767.0, step: 1.0 }, "0".into(), Vec::new(), false)),
                Maker::Canon if self.info.supports(CANON_DRIVE_LENS) => {
                    let mut choices: Vec<String> = CANON_DRIVE_CHOICES[..3].iter().map(|(n, _)| n.to_string()).collect();
                    choices.push("None".into());
                    choices.extend(CANON_DRIVE_CHOICES[3..].iter().map(|(n, _)| n.to_string()));
                    Some(setting(Kind::Choice, "None".into(), choices, false))
                }
                _ => None,
            },
            _ => {
                let (code, words) = self.menu(name)?;
                let (writable, list, current) = self.menu_values(code)?;
                let mut choices: Vec<String> = Vec::new();
                for raw in &list {
                    let label = words.label(*raw);
                    if !choices.contains(&label) {
                        choices.push(label);
                    }
                }
                let value = current.map(|raw| words.label(raw)).unwrap_or_default();
                Some(setting(Kind::Choice, value, choices, !writable))
            }
        }
    }

    fn write(&mut self, name: &str, value: &str) -> Result<(), String> {
        match name {
            "viewfinder" => {
                if value == "1" || value == "true" {
                    // Nothing is started for a camera that is being let go of
                    if self.is_stopped() {
                        return Err("stopped".into());
                    }
                    self.start_live_view()
                } else {
                    self.end_live_view();
                    Ok(())
                }
            }
            "autofocusdrive" => match self.maker {
                Maker::Nikon => {
                    self.busy_run(NIKON_AF_DRIVE, &[])?;
                    // The lens is still moving until the camera says it is ready
                    self.busy_run(NIKON_DEVICE_READY, &[]).map(|_| ())
                }
                Maker::Canon => self.busy_run(CANON_DO_AF, &[]).map(|_| ()),
                Maker::Other => Err("this camera's autofocus isn't known here".into()),
            },
            "manualfocusdrive" => match self.maker {
                Maker::Nikon => {
                    let steps: i32 = value.parse().map_err(|_| format!("{value} isn't a number"))?;
                    // Direction 1 moves toward near, 2 toward far
                    self.busy_run(NIKON_MF_DRIVE, &[if steps < 0 { 1 } else { 2 }, steps.unsigned_abs()]).map(|_| ())
                }
                Maker::Canon => {
                    let step = CANON_DRIVE_CHOICES.iter().find(|(n, _)| *n == value).ok_or_else(|| format!("{value} isn't a focus step"))?.1;
                    self.busy_run(CANON_DRIVE_LENS, &[step]).map(|_| ())
                }
                Maker::Other => Err("this camera's focus drive isn't known here".into()),
            },
            _ => {
                let (code, words) = self.menu(name).ok_or("no such setting")?;
                let (writable, list, _) = self.menu_values(code).ok_or("the camera doesn't have that setting")?;
                if !writable {
                    return Err("the camera doesn't allow it now".into());
                }
                let raw = list.into_iter().find(|raw| words.label(*raw) == value).ok_or_else(|| format!("{value} isn't one of its choices"))?;
                if self.maker == Maker::Canon {
                    self.canon_set(code as u32, raw as u32)?;
                    self.canon.entry(code).or_default().value = Some(raw);
                    Ok(())
                } else {
                    self.set_raw(code, raw)
                }
            }
        }
    }

    fn set_stop(&mut self, stop: Arc<AtomicBool>) {
        self.stop = Some(stop);
    }

    fn capture(&mut self, expect: usize) -> Result<Vec<Shot>, Incomplete> {
        self.take_picture(expect)
    }

    fn restart_live_view(&mut self) {
        self.end_live_view();
        let _ = self.start_live_view();
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if self.live {
            self.end_live_view();
        }
        if self.maker == Maker::Canon {
            let _ = self.t.transact(CANON_SET_REMOTE_MODE, &[0], None);
        }
        let _ = self.t.transact(ptp::OP_CLOSE_SESSION, &[], None);
    }
}

impl Drop for PtpCamera {
    fn drop(&mut self) {
        self.close();
    }
}

mod capture;

#[cfg(test)]
mod tests;
