//! PTP, the protocol cameras speak over USB, as far as live view and settings need it: what a transport carries (one operation with its
//! parameters and an optional data phase, answered by a response code, parameters and data), and the datasets read from the camera
//! (device info, property descriptions). Where the bytes travel is a `Transport`: Windows Portable Devices on Windows (ptp_wpd.rs) and a
//! mock in the tests, so everything above it (ptp_rig.rs) runs on any system.

/// Operations every PTP camera has.
pub const OP_GET_DEVICE_INFO: u16 = 0x1001;
pub const OP_OPEN_SESSION: u16 = 0x1002;
pub const OP_CLOSE_SESSION: u16 = 0x1003;
pub const OP_GET_DEVICE_PROP_DESC: u16 = 0x1014;
pub const OP_GET_DEVICE_PROP_VALUE: u16 = 0x1015;
pub const OP_SET_DEVICE_PROP_VALUE: u16 = 0x1016;

/// Response codes.
pub const RC_OK: u16 = 0x2001;
pub const RC_DEVICE_BUSY: u16 = 0x2019;
pub const RC_SESSION_ALREADY_OPEN: u16 = 0x201E;

/// Vendor extension ids a camera names in its device info.
pub const VENDOR_NIKON: u32 = 0x0000_000A;
pub const VENDOR_CANON: u32 = 0x0000_000B;

/// What a camera answered one operation with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub params: Vec<u32>,
    pub data: Vec<u8>,
}

impl Reply {
    pub fn ok(&self) -> bool {
        self.code == RC_OK
    }
}

/// A line to one camera. One transaction at a time, from one thread.
pub trait Transport: Send {
    /// Runs one operation: `data_out` is the data phase sent to the camera, if the operation has one. Err only when the camera couldn't be
    /// reached at all; a camera that says no answers with a response code other than RC_OK.
    fn transact(&mut self, op: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Reply, String>;
}

/// Whether an operation answers with a data phase, for a transport that has to be told (Windows Portable Devices has one command for
/// operations that send data back and another for those that don't). The operations the rig uses that do: device info, a property's
/// description and value, Nikon's live view frame, Canon's viewfinder frame and Canon's events.
pub fn returns_data(op: u16) -> bool {
    matches!(op, OP_GET_DEVICE_INFO | OP_GET_DEVICE_PROP_DESC | OP_GET_DEVICE_PROP_VALUE | 0x9203 | 0x9153 | 0x9116)
}

/// Runs an operation and turns a response other than OK into an error naming the operation.
pub fn run(t: &mut dyn Transport, op: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Reply, String> {
    let reply = t.transact(op, params, data_out)?;
    if reply.ok() {
        Ok(reply)
    } else {
        Err(format!("the camera refused operation {op:#06x} ({:#06x})", reply.code))
    }
}

/// Reads PTP's little-endian fields out of a buffer; a read past the end gives None and the dataset is dropped.
pub struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let part = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(part)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
    }

    /// A PTP string: a count of UTF-16 units including the ending zero (0 for none), then the units.
    pub fn string(&mut self) -> Option<String> {
        let count = self.u8()? as usize;
        let units: Vec<u16> = (0..count).map(|_| self.u16()).collect::<Option<_>>()?;
        Some(String::from_utf16_lossy(units.strip_suffix(&[0]).unwrap_or(&units)))
    }

    /// An array of u16: a u32 count, then the values.
    pub fn array_u16(&mut self) -> Option<Vec<u16>> {
        let count = self.u32()? as usize;
        // A count the rest of the buffer can't hold is a broken dataset, not a reason to allocate
        if count > self.bytes.len().saturating_sub(self.at) / 2 {
            return None;
        }
        (0..count).map(|_| self.u16()).collect()
    }
}

/// Builds the same fields, for a camera that takes a value (a property to set).
pub fn put_string(out: &mut Vec<u8>, text: &str) {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    out.push(units.len().min(255) as u8);
    units.iter().take(255).for_each(|u| out.extend_from_slice(&u.to_le_bytes()));
}

/// What GetDeviceInfo tells about the camera, the part used here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub vendor_extension: u32,
    pub operations: Vec<u16>,
    pub properties: Vec<u16>,
    pub manufacturer: String,
    pub model: String,
}

impl DeviceInfo {
    pub fn parse(data: &[u8]) -> Option<DeviceInfo> {
        let mut c = Cursor::new(data);
        c.u16()?; // standard version
        let vendor_extension = c.u32()?;
        c.u16()?; // vendor extension version
        c.string()?; // vendor extension description
        c.u16()?; // functional mode
        let operations = c.array_u16()?;
        c.array_u16()?; // events
        let properties = c.array_u16()?;
        c.array_u16()?; // capture formats
        c.array_u16()?; // playback formats
        let manufacturer = c.string()?;
        let model = c.string()?;
        Some(DeviceInfo { vendor_extension, operations, properties, manufacturer, model })
    }

    pub fn supports(&self, op: u16) -> bool {
        self.operations.contains(&op)
    }
}

/// The makers whose live view commands are known here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    Nikon,
    Canon,
    Other,
}

impl DeviceInfo {
    pub fn maker(&self) -> Maker {
        let name = self.manufacturer.to_ascii_lowercase();
        if self.vendor_extension == VENDOR_NIKON || name.contains("nikon") {
            Maker::Nikon
        } else if self.vendor_extension == VENDOR_CANON || name.contains("canon") {
            Maker::Canon
        } else {
            Maker::Other
        }
    }
}

/// A property's value as the camera types it: PTP data types 1 to 8 are integers of 8 to 64 bits, 0xFFFF a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Text(String),
}

impl Value {
    pub fn int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Text(_) => None,
        }
    }
}

/// The data type of a property, as the camera numbers it.
pub const TYPE_STRING: u16 = 0xFFFF;

fn read_value(c: &mut Cursor, kind: u16) -> Option<Value> {
    Some(Value::Int(match kind {
        1 => c.u8()? as i8 as i64,
        2 => c.u8()? as i64,
        3 => c.u16()? as i16 as i64,
        4 => c.u16()? as i64,
        5 => c.u32()? as i32 as i64,
        6 => c.u32()? as i64,
        7 | 8 => c.u64()? as i64,
        TYPE_STRING => return Some(Value::Text(c.string()?)),
        _ => return None,
    }))
}

/// A value as the bytes the camera takes for a property of this type.
pub fn encode_value(kind: u16, value: &Value) -> Option<Vec<u8>> {
    match (kind, value) {
        (1 | 2, Value::Int(n)) => Some(vec![*n as u8]),
        (3 | 4, Value::Int(n)) => Some((*n as u16).to_le_bytes().to_vec()),
        (5 | 6, Value::Int(n)) => Some((*n as u32).to_le_bytes().to_vec()),
        (7 | 8, Value::Int(n)) => Some((*n as u64).to_le_bytes().to_vec()),
        (TYPE_STRING, Value::Text(t)) => {
            let mut out = Vec::new();
            put_string(&mut out, t);
            Some(out)
        }
        _ => None,
    }
}

/// What values a property takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Form {
    Any,
    Range { min: i64, max: i64, step: i64 },
    Enum(Vec<Value>),
}

/// A property as GetDevicePropDesc describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropDesc {
    pub code: u16,
    pub kind: u16,
    pub writable: bool,
    pub current: Value,
    pub form: Form,
}

impl PropDesc {
    pub fn parse(data: &[u8]) -> Option<PropDesc> {
        let mut c = Cursor::new(data);
        let code = c.u16()?;
        let kind = c.u16()?;
        let writable = c.u8()? == 1;
        read_value(&mut c, kind)?; // factory default
        let current = read_value(&mut c, kind)?;
        let form = match c.u8()? {
            1 => {
                let (min, max, step) = (read_value(&mut c, kind)?.int()?, read_value(&mut c, kind)?.int()?, read_value(&mut c, kind)?.int()?);
                Form::Range { min, max, step }
            }
            2 => {
                let count = c.u16()? as usize;
                Form::Enum((0..count).map(|_| read_value(&mut c, kind)).collect::<Option<_>>()?)
            }
            _ => Form::Any,
        };
        Some(PropDesc { code, kind, writable, current, form })
    }
}

/// The bytes of a JPEG inside a longer buffer (live view frames come with a header of the maker's own), from its start marker to its
/// last end marker. None when there isn't one.
pub fn find_jpeg(data: &[u8]) -> Option<&[u8]> {
    let start = data.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF])?;
    let end = data.windows(2).rposition(|w| w == [0xFF, 0xD9]).filter(|e| *e > start)?;
    Some(&data[start..end + 2])
}

#[cfg(test)]
pub mod mock;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn only_the_operations_that_answer_with_data_say_so() {
        assert!([OP_GET_DEVICE_INFO, OP_GET_DEVICE_PROP_DESC, 0x9203, 0x9153, 0x9116].iter().all(|op| returns_data(*op)));
        assert!(![OP_OPEN_SESSION, OP_SET_DEVICE_PROP_VALUE, 0x9201, 0x9204, 0x9110].iter().any(|op| returns_data(*op)));
    }

    #[test]
    fn strings_round_trip_and_an_empty_one_is_a_single_zero() {
        assert_eq!(Cursor::new(&string("Nikon Corporation")).string().as_deref(), Some("Nikon Corporation"));
        assert_eq!(string(""), vec![1, 0, 0]);
        assert_eq!(Cursor::new(&[0]).string().as_deref(), Some(""));
        // A count the buffer can't hold
        assert_eq!(Cursor::new(&[5, 65, 0]).string(), None);
    }

    #[test]
    fn device_info_gives_the_maker_the_model_and_what_it_can_do() {
        let mut data = 100u16.to_le_bytes().to_vec();
        data.extend_from_slice(&VENDOR_NIKON.to_le_bytes());
        data.extend_from_slice(&100u16.to_le_bytes());
        data.extend(string("Nikon"));
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend(array(&[0x1001, 0x9201]));
        data.extend(array(&[]));
        data.extend(array(&[0x5007]));
        data.extend(array(&[]));
        data.extend(array(&[]));
        data.extend(string("Nikon Corporation"));
        data.extend(string("Nikon test camera"));
        let info = DeviceInfo::parse(&data).unwrap();
        assert_eq!(info.maker(), Maker::Nikon);
        assert_eq!(info.model, "Nikon test camera");
        assert!(info.supports(0x9201) && !info.supports(0x9202));
        assert_eq!(info.properties, vec![0x5007]);
        assert_eq!(DeviceInfo::parse(&data[..data.len() - 3]), None);
    }

    #[test]
    fn the_maker_comes_from_the_name_when_the_extension_id_isnt_one_we_know() {
        let info = |ext, name: &str| DeviceInfo { vendor_extension: ext, manufacturer: name.into(), ..DeviceInfo::default() };
        assert_eq!(info(0, "Canon Inc.").maker(), Maker::Canon);
        assert_eq!(info(VENDOR_CANON, "").maker(), Maker::Canon);
        assert_eq!(info(0x6, "Microsoft").maker(), Maker::Other);
    }

    #[test]
    fn a_property_description_reads_current_value_and_choices() {
        let mut data = 0x5007u16.to_le_bytes().to_vec();
        data.extend_from_slice(&4u16.to_le_bytes());
        data.push(1);
        data.extend_from_slice(&280u16.to_le_bytes());
        data.extend_from_slice(&560u16.to_le_bytes());
        data.push(2);
        data.extend_from_slice(&3u16.to_le_bytes());
        for v in [280u16, 400, 560] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let desc = PropDesc::parse(&data).unwrap();
        assert_eq!((desc.code, desc.writable, desc.current.clone()), (0x5007, true, Value::Int(560)));
        assert_eq!(desc.form, Form::Enum(vec![Value::Int(280), Value::Int(400), Value::Int(560)]));
        assert_eq!(PropDesc::parse(&data[..data.len() - 1]), None);
    }

    #[test]
    fn a_range_property_and_a_signed_one() {
        let mut data = 0x5010u16.to_le_bytes().to_vec();
        data.extend_from_slice(&3u16.to_le_bytes());
        data.push(0);
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&(-333i16).to_le_bytes());
        data.push(1);
        for v in [-5000i16, 5000, 333] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let desc = PropDesc::parse(&data).unwrap();
        assert_eq!((desc.writable, desc.current.clone()), (false, Value::Int(-333)));
        assert_eq!(desc.form, Form::Range { min: -5000, max: 5000, step: 333 });
        assert_eq!(encode_value(3, &Value::Int(-333)).unwrap(), (-333i16).to_le_bytes().to_vec());
        assert_eq!(encode_value(4, &Value::Text("x".into())), None);
    }

    #[test]
    fn a_jpeg_is_found_after_the_makers_header() {
        let mut frame = vec![9u8; 64];
        frame.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9]);
        frame.extend_from_slice(&[0, 0, 0]);
        assert_eq!(find_jpeg(&frame).unwrap(), &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9]);
        assert_eq!(find_jpeg(&[1, 2, 3]), None);
        assert_eq!(find_jpeg(&[0xFF, 0xD8, 0xFF, 1]), None);
    }
}
