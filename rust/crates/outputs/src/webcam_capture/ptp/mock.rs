//! A pretend Nikon or Canon on the other end of a Transport: it keeps the state the real ones do (a session, live view on or off, settings)
//! and answers the operations the rig sends the way libgphoto2's drivers expect them answered, so the rig can be run end to end without a camera.

use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A JPEG small enough to write out: only its markers matter to the rig.
pub const JPEG: [u8; 8] = [0xFF, 0xD8, 0xFF, 0xE0, 7, 7, 0xFF, 0xD9];

#[derive(Default)]
pub struct State {
    pub maker: Option<Maker>,
    pub session: bool,
    pub live: bool,
    /// Operations the camera lists as supported; the rest are refused.
    pub operations: Vec<u16>,
    /// Standard properties: description bytes by code.
    pub props: HashMap<u16, PropDesc>,
    /// Canon: the properties it reports through events, with the values on offer.
    pub canon_props: HashMap<u16, (u32, Vec<u32>)>,
    /// How many frame requests answer busy before one works.
    pub busy_frames: u32,
    /// Every operation received, with its parameters and data.
    pub log: Vec<(u16, Vec<u32>, Vec<u8>)>,
    pub frames: u32,
    /// Answer every live view frame with this response instead (the camera ended live view).
    pub frame_code: Option<u16>,
    pub unreachable: bool,
    /// How many viewfinder requests answer Canon's not-ready code before one works.
    pub not_ready_frames: u32,
    /// The files a photo makes: name and bytes. One JPEG unless a test sets more.
    pub photo_files: Vec<(String, Vec<u8>)>,
    /// A photo that is taken but never reported, as when the camera is stuck.
    pub silent_photos: bool,
    /// Nikon's pending events, read by the computer.
    pub nikon_events: Vec<(u16, u32)>,
    /// Files on the camera by handle.
    pub objects: HashMap<u32, (String, Vec<u8>)>,
    /// Canon's file announcements not yet read: handle, name and whether it is an offer to send.
    pub canon_announced: Vec<(u32, String, bool)>,
    /// The Nikon puts its photo on the card and announces it as a file added there.
    pub to_card: bool,
    /// A folder the Nikon announces before its photo.
    pub folder_first: bool,
    /// Handles that are folders.
    pub folders: std::collections::HashSet<u32>,
    /// The most data one reply may carry; a larger one fails as it does over a transport that caps a transfer.
    pub max_data: Option<usize>,
    /// What the camera answers a delete with, in place of success.
    pub delete_code: Option<u16>,
    /// What the camera answers a release of the shutter button with, in place of success.
    pub release_code: Option<u16>,
    /// How many readiness checks answer busy before one works.
    pub ready_busy: u32,
    /// Live view can't be ended: the camera says it is busy.
    pub end_busy: bool,
    /// The model name, when it isn't the usual one.
    pub model: Option<String>,
}

#[derive(Clone, Default)]
pub struct Mock(pub Arc<Mutex<State>>);

fn string(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_string(&mut out, text);
    out
}

fn array(values: &[u16]) -> Vec<u8> {
    let mut out = (values.len() as u32).to_le_bytes().to_vec();
    values.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
    out
}

fn prop_desc(desc: &PropDesc) -> Vec<u8> {
    let mut out = desc.code.to_le_bytes().to_vec();
    out.extend_from_slice(&desc.kind.to_le_bytes());
    out.push(desc.writable as u8);
    let value = |v: &Value| encode_value(desc.kind, v).unwrap();
    out.extend(value(&desc.current));
    out.extend(value(&desc.current));
    match &desc.form {
        Form::Any => out.push(0),
        Form::Range { min, max, step } => {
            out.push(1);
            for v in [min, max, step] {
                out.extend(value(&Value::Int(*v)));
            }
        }
        Form::Enum(values) => {
            out.push(2);
            out.extend_from_slice(&(values.len() as u16).to_le_bytes());
            values.iter().for_each(|v| out.extend(value(v)));
        }
    }
    out
}

impl Mock {
    pub fn nikon() -> Mock {
        let mock = Mock::default();
        {
            let mut s = mock.0.lock().unwrap();
            s.maker = Some(Maker::Nikon);
            s.operations = vec![0x1001, 0x1002, 0x1003, 0x1008, 0x1009, 0x100B, 0x101B, 0x1014, 0x1015, 0x1016, 0x90C0, 0x90C1, 0x90C7, 0x90C8, 0x9201, 0x9202, 0x9203, 0x9204];
            s.photo_files = vec![("DSC_0001.JPG".into(), JPEG.to_vec())];
            let desc = |code, kind, current, form| PropDesc { code, kind, writable: true, current: Value::Int(current), form };
            let ints = |v: &[i64]| Form::Enum(v.iter().map(|n| Value::Int(*n)).collect());
            s.props.insert(0x5007, desc(0x5007, 4, 560, ints(&[280, 400, 560, 800])));
            s.props.insert(0x5010, desc(0x5010, 3, 0, Form::Range { min: -1000, max: 1000, step: 333 }));
            s.props.insert(0x5005, desc(0x5005, 4, 2, ints(&[2, 4, 6, 0x8010])));
            s.props.insert(0xD10B, desc(0xD10B, 2, 0, ints(&[0, 1])));
            s.props.insert(0x5004, desc(0x5004, 2, 1, ints(&[1, 2, 4, 7])));
        }
        mock
    }

    pub fn canon() -> Mock {
        let mock = Mock::default();
        {
            let mut s = mock.0.lock().unwrap();
            s.maker = Some(Maker::Canon);
            s.operations = vec![0x1001, 0x1002, 0x1003, 0x1008, 0x1009, 0x100B, 0x101B, 0x9110, 0x9114, 0x9115, 0x9116, 0x9117, 0x9128, 0x9129, 0x9153, 0x9154, 0x9155];
            s.photo_files = vec![("IMG_0001.JPG".into(), JPEG.to_vec())];
            s.canon_props.insert(0xD11C, (2, vec![2, 4]));
            // f/4 (0x28), f/5.6 (0x30), f/8 (0x38); exposure compensation 0, +1/3, +2/3 (as eighths); daylight and auto
            s.canon_props.insert(0xD101, (0x30, vec![0x28, 0x30, 0x38]));
            s.canon_props.insert(0xD104, (0, vec![0xFD, 0, 3, 5]));
            s.canon_props.insert(0xD10B, (0, vec![0, 1, 2]));
        }
        mock
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    pub fn ops(&self) -> Vec<u16> {
        self.state().log.iter().map(|(op, _, _)| *op).collect()
    }
}

fn reply(code: u16, data: Vec<u8>) -> Result<Reply, String> {
    Ok(Reply { code, params: Vec::new(), data })
}

impl Transport for Mock {
    fn transact(&mut self, op: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Reply, String> {
        let mut s = self.0.lock().unwrap();
        if s.unreachable {
            return Err("the camera isn't there".into());
        }
        s.log.push((op, params.to_vec(), data_out.map(<[u8]>::to_vec).unwrap_or_default()));
        if op != OP_OPEN_SESSION && !s.session && op != OP_GET_DEVICE_INFO {
            return reply(0x2003, Vec::new());
        }
        if s.maker.is_none() || !(s.operations.contains(&op)) {
            return reply(0x2005, Vec::new());
        }
        if let (Some(max), true) = (s.max_data, matches!(op, 0x1009)) {
            if s.objects.get(&params[0]).is_some_and(|(_, d)| d.len() > max) {
                return Err("the transfer is larger than the transport allows".into());
            }
        }
        match op {
            OP_OPEN_SESSION => {
                let already = std::mem::replace(&mut s.session, true);
                reply(if already { RC_SESSION_ALREADY_OPEN } else { RC_OK }, Vec::new())
            }
            OP_CLOSE_SESSION => {
                s.session = false;
                reply(RC_OK, Vec::new())
            }
            OP_GET_DEVICE_INFO => {
                let custom = s.model.clone();
                let (vendor, name, model) = match s.maker {
                    Some(Maker::Canon) => (VENDOR_CANON, "Canon Inc.", "Canon test camera"),
                    Some(Maker::Other) => (0x6, "Acme", "Acme test camera"),
                    _ => (VENDOR_NIKON, "Nikon Corporation", "Nikon test camera"),
                };
                let mut data = 100u16.to_le_bytes().to_vec();
                data.extend_from_slice(&vendor.to_le_bytes());
                data.extend_from_slice(&100u16.to_le_bytes());
                data.extend(string(""));
                data.extend_from_slice(&0u16.to_le_bytes());
                data.extend(array(&s.operations));
                data.extend(array(&[]));
                let mut props: Vec<u16> = s.props.keys().copied().collect();
                props.sort_unstable();
                data.extend(array(&props));
                data.extend(array(&[]));
                data.extend(array(&[]));
                data.extend(string(name));
                data.extend(string(custom.as_deref().unwrap_or(model)));
                reply(RC_OK, data)
            }
            OP_GET_DEVICE_PROP_DESC => match s.props.get(&(params[0] as u16)) {
                Some(desc) => reply(RC_OK, prop_desc(desc)),
                None => reply(0x200A, Vec::new()),
            },
            OP_SET_DEVICE_PROP_VALUE => {
                let code = params[0] as u16;
                let Some(desc) = s.props.get_mut(&code) else { return reply(0x200A, Vec::new()) };
                let mut c = Cursor::new(data_out.unwrap_or_default());
                match read_value(&mut c, desc.kind) {
                    Some(v) => {
                        desc.current = v;
                        reply(RC_OK, Vec::new())
                    }
                    None => reply(0x201B, Vec::new()),
                }
            }
            // Nikon
            0x9201 => {
                s.live = true;
                reply(RC_OK, Vec::new())
            }
            0x9202 => {
                if s.end_busy {
                    return reply(RC_DEVICE_BUSY, Vec::new());
                }
                s.live = false;
                reply(RC_OK, Vec::new())
            }
            0x9203 | 0x9153 => {
                if let Some(code) = s.frame_code {
                    return reply(code, Vec::new());
                }
                let started = if op == 0x9203 { s.live } else { s.canon_props.contains_key(&0xD1B0) && s.live };
                if !started {
                    return reply(0xA00B, Vec::new());
                }
                if s.not_ready_frames > 0 {
                    s.not_ready_frames -= 1;
                    return reply(0xA102, Vec::new());
                }
                if s.busy_frames > 0 {
                    s.busy_frames -= 1;
                    return reply(RC_DEVICE_BUSY, Vec::new());
                }
                s.frames += 1;
                let mut frame = vec![0u8; 64];
                frame.extend_from_slice(&JPEG);
                frame.extend_from_slice(&[0; 5]);
                reply(RC_OK, frame)
            }
            0x90C8 if s.ready_busy > 0 => {
                s.ready_busy -= 1;
                reply(RC_DEVICE_BUSY, Vec::new())
            }
            0x9129 if s.release_code.is_some() => reply(s.release_code.unwrap_or(RC_OK), Vec::new()),
            0x100B => {
                let handle = params[0];
                if let Some(code) = s.delete_code {
                    return reply(code, Vec::new());
                }
                match s.objects.remove(&handle) {
                    Some(_) => reply(RC_OK, Vec::new()),
                    None => reply(0x2009, Vec::new()),
                }
            }
            0x101B => match s.objects.get(&params[0]) {
                Some((_, data)) => {
                    let (at, len) = (params[1] as usize, params[2] as usize);
                    let end = (at + len).min(data.len());
                    reply(RC_OK, data.get(at..end).unwrap_or_default().to_vec())
                }
                None => reply(0x2009, Vec::new()),
            },
            0x90C1 | 0x90C8 | 0x9204 | 0x9154 | 0x9155 | 0x9114 | 0x9115 | 0x9117 | 0x9129 => reply(RC_OK, Vec::new()),
            // Nikon: take a photo into the camera's memory, say so in events
            0x90C0 => {
                if !s.silent_photos {
                    if s.folder_first {
                        s.folders.insert(0x10);
                        s.objects.insert(0x10, ("100NIKON".into(), Vec::new()));
                        s.nikon_events.push((0x4002, 0x10));
                    }
                    for (i, (name, data)) in s.photo_files.clone().into_iter().enumerate() {
                        let handle = 0xFFFF_0001 + i as u32;
                        s.objects.insert(handle, (name, data));
                        let code = if s.to_card { 0x4002 } else { 0xC101 };
                        s.nikon_events.push((code, handle));
                    }
                    s.nikon_events.push((0xC102, 0));
                }
                reply(RC_OK, Vec::new())
            }
            0x90C7 => {
                let events = std::mem::take(&mut s.nikon_events);
                let mut data = (events.len() as u16).to_le_bytes().to_vec();
                for (code, param) in events {
                    data.extend_from_slice(&code.to_le_bytes());
                    data.extend_from_slice(&param.to_le_bytes());
                }
                reply(RC_OK, data)
            }
            0x1008 | 0x1009 => match s.objects.get(&params[0]) {
                Some((_, data)) if op == 0x1009 => reply(RC_OK, data.clone()),
                Some((name, data)) => {
                    let mut info = vec![0u8; 52];
                    if s.folders.contains(&params[0]) {
                        info[4..6].copy_from_slice(&0x3001u16.to_le_bytes());
                    }
                    info[8..12].copy_from_slice(&(data.len() as u32).to_le_bytes());
                    put_string(&mut info, name);
                    reply(RC_OK, info)
                }
                None => reply(0x2009, Vec::new()),
            },
            // Canon: the full press takes the photo, and the camera announces its files
            0x9128 => {
                if matches!(params[0], 2 | 3) && !s.silent_photos {
                    // With the computer as the destination the camera offers to send each file, otherwise it says the file is on its card
                    let to_computer = s.canon_props.get(&0xD11C).is_some_and(|p| p.0 == 4);
                    for (i, (name, data)) in s.photo_files.clone().into_iter().enumerate() {
                        let handle = 0x1000 + i as u32;
                        s.objects.insert(handle, (name.clone(), data));
                        s.canon_announced.push((handle, name, to_computer));
                    }
                }
                reply(RC_OK, Vec::new())
            }
            // Canon: set a property, read what changed
            0x9110 => {
                let data = data_out.unwrap_or_default();
                let (prop, value) = (read_u32(data, 4).unwrap_or(0), read_u32(data, 8).unwrap_or(0));
                if prop == 0xD1B0 {
                    s.live = value == 2;
                    s.canon_props.insert(0xD1B0, (value, Vec::new()));
                } else if let Some(entry) = s.canon_props.get_mut(&(prop as u16)) {
                    entry.0 = value;
                }
                reply(RC_OK, Vec::new())
            }
            0x9116 => {
                let mut data = Vec::new();
                let mut record = |kind: u32, body: Vec<u8>| {
                    data.extend_from_slice(&(8 + body.len() as u32).to_le_bytes());
                    data.extend_from_slice(&kind.to_le_bytes());
                    data.extend(body);
                };
                let mut codes: Vec<&u16> = s.canon_props.keys().filter(|c| **c != 0xD1B0).collect();
                codes.sort();
                for code in codes {
                    let (value, list) = &s.canon_props[code];
                    let mut body = (*code as u32).to_le_bytes().to_vec();
                    body.extend_from_slice(&value.to_le_bytes());
                    record(0xC189, body);
                    let mut body = (*code as u32).to_le_bytes().to_vec();
                    body.extend_from_slice(&3u32.to_le_bytes());
                    body.extend_from_slice(&(list.len() as u32).to_le_bytes());
                    list.iter().for_each(|v| body.extend_from_slice(&v.to_le_bytes()));
                    record(0xC18A, body);
                }
                for (handle, name, offer) in std::mem::take(&mut s.canon_announced) {
                    // An offer to send has the format at 0x0c and the name at 0x1c, a file on the card the format at 0x10 and the name at
                    // 0x28 (offsets from the record's start, header included)
                    let (format_at, name_at, kind) = if offer { (4, 0x1c - 8, 0xC186) } else { (8, 0x28 - 8, 0xC181) };
                    let mut body = vec![0u8; name_at];
                    body[0..4].copy_from_slice(&handle.to_le_bytes());
                    body[format_at..format_at + 2].copy_from_slice(&0x3801u16.to_le_bytes());
                    body.extend_from_slice(name.as_bytes());
                    body.push(0);
                    record(kind, body);
                }
                record(0, Vec::new());
                reply(RC_OK, data)
            }
            _ => reply(0x2005, Vec::new()),
        }
    }
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
