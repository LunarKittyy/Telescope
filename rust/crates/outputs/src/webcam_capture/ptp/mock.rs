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
            s.operations = vec![0x1001, 0x1002, 0x1003, 0x1014, 0x1015, 0x1016, 0x90C1, 0x90C8, 0x9201, 0x9202, 0x9203, 0x9204];
            let desc = |code, kind, current, form| PropDesc { code, kind, writable: true, current: Value::Int(current), form };
            let ints = |v: &[i64]| Form::Enum(v.iter().map(|n| Value::Int(*n)).collect());
            s.props.insert(0x5007, desc(0x5007, 4, 560, ints(&[280, 400, 560, 800])));
            s.props.insert(0x5010, desc(0x5010, 3, 0, Form::Range { min: -1000, max: 1000, step: 333 }));
            s.props.insert(0x5005, desc(0x5005, 4, 2, ints(&[2, 4, 6, 0x8010])));
            s.props.insert(0xD10B, desc(0xD10B, 2, 0, ints(&[0, 1])));
        }
        mock
    }

    pub fn canon() -> Mock {
        let mock = Mock::default();
        {
            let mut s = mock.0.lock().unwrap();
            s.maker = Some(Maker::Canon);
            s.operations = vec![0x1001, 0x1002, 0x1003, 0x9110, 0x9114, 0x9115, 0x9116, 0x9153, 0x9154, 0x9155];
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
                data.extend(string(model));
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
            0x90C1 | 0x90C8 | 0x9204 | 0x9154 | 0x9155 | 0x9114 | 0x9115 => reply(RC_OK, Vec::new()),
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
