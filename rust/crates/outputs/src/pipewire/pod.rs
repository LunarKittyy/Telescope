//! The few SPA pods the video streams need, written and read in plain Rust: a pod is a size, a type and a body padded to 8 bytes.

pub const TYPE_ID: u32 = 3;
pub const TYPE_INT: u32 = 4;
pub const TYPE_LONG: u32 = 5;
pub const TYPE_RECTANGLE: u32 = 10;
pub const TYPE_FRACTION: u32 = 11;
pub const TYPE_OBJECT: u32 = 15;
pub const TYPE_CHOICE: u32 = 19;

pub const OBJECT_FORMAT: u32 = 0x40003;
pub const OBJECT_PARAM_BUFFERS: u32 = 0x40004;

pub const PARAM_ENUM_FORMAT: u32 = 3;
pub const PARAM_FORMAT: u32 = 4;
pub const PARAM_BUFFERS: u32 = 5;

pub const FORMAT_MEDIA_TYPE: u32 = 1;
pub const FORMAT_MEDIA_SUBTYPE: u32 = 2;
pub const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
pub const FORMAT_VIDEO_MODIFIER: u32 = 0x20002;
pub const FORMAT_VIDEO_SIZE: u32 = 0x20003;
pub const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;

pub const BUFFERS_BUFFERS: u32 = 1;
pub const BUFFERS_BLOCKS: u32 = 2;
pub const BUFFERS_SIZE: u32 = 3;
pub const BUFFERS_STRIDE: u32 = 4;
pub const BUFFERS_ALIGN: u32 = 5;
pub const BUFFERS_DATA_TYPE: u32 = 6;

pub const MEDIA_TYPE_VIDEO: u32 = 2;
pub const MEDIA_SUBTYPE_RAW: u32 = 1;

pub const PROP_MANDATORY: u32 = 1 << 3;
pub const PROP_DONT_FIXATE: u32 = 1 << 4;

/// A single value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    Id(u32),
    Int(i32),
    Long(i64),
    Rect(u32, u32),
    Frac(u32, u32),
}

impl Scalar {
    fn kind(self) -> u32 {
        match self {
            Scalar::Id(_) => TYPE_ID,
            Scalar::Int(_) => TYPE_INT,
            Scalar::Long(_) => TYPE_LONG,
            Scalar::Rect(..) => TYPE_RECTANGLE,
            Scalar::Frac(..) => TYPE_FRACTION,
        }
    }

    fn bytes(self) -> Vec<u8> {
        match self {
            Scalar::Id(v) => v.to_ne_bytes().to_vec(),
            Scalar::Int(v) => v.to_ne_bytes().to_vec(),
            Scalar::Long(v) => v.to_ne_bytes().to_vec(),
            Scalar::Rect(a, b) | Scalar::Frac(a, b) => [a.to_ne_bytes(), b.to_ne_bytes()].concat(),
        }
    }

    fn read(kind: u32, body: &[u8]) -> Option<Scalar> {
        let u32_at = |at: usize| body.get(at..at + 4).map(|b| u32::from_ne_bytes(b.try_into().unwrap()));
        match kind {
            TYPE_ID => Some(Scalar::Id(u32_at(0)?)),
            TYPE_INT => Some(Scalar::Int(u32_at(0)? as i32)),
            TYPE_LONG => Some(Scalar::Long(i64::from_ne_bytes(body.get(..8)?.try_into().unwrap()))),
            TYPE_RECTANGLE => Some(Scalar::Rect(u32_at(0)?, u32_at(4)?)),
            TYPE_FRACTION => Some(Scalar::Frac(u32_at(0)?, u32_at(4)?)),
            _ => None,
        }
    }
}

/// How a choice's values read (spa_choice_type): the first is the default and the rest what else is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    None = 0,
    /// Default, minimum, maximum.
    Range = 1,
    Enum = 3,
    Flags = 4,
}

/// A property's value: one thing or a choice of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    One(Scalar),
    Choice(Choice, Vec<Scalar>),
}

impl Value {
    /// The value to use: the one itself, or a choice's default.
    pub fn default_value(&self) -> Option<Scalar> {
        match self {
            Value::One(v) => Some(*v),
            Value::Choice(_, values) => values.first().copied(),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Value::One(v) => {
                let body = v.bytes();
                header(out, body.len() as u32, v.kind());
                out.extend(body);
                pad(out);
            }
            Value::Choice(kind, values) => {
                let first = values.first().copied().unwrap_or(Scalar::Int(0));
                let child = first.bytes().len() as u32;
                header(out, 16 + child * values.len() as u32, TYPE_CHOICE);
                out.extend((*kind as u32).to_ne_bytes());
                out.extend(0u32.to_ne_bytes());
                header(out, child, first.kind());
                for v in values {
                    out.extend(v.bytes());
                }
                pad(out);
            }
        }
    }
}

/// One property of an object pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prop {
    pub key: u32,
    pub flags: u32,
    pub value: Value,
}

fn header(out: &mut Vec<u8>, size: u32, kind: u32) {
    out.extend(size.to_ne_bytes());
    out.extend(kind.to_ne_bytes());
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
}

/// A finished pod, kept 8-byte aligned the way libspa wants it.
pub struct Pod(Vec<u64>);

impl Pod {
    /// An object pod (a Format or a Buffers param) from its properties.
    pub fn object(kind: u32, id: u32, props: &[Prop]) -> Pod {
        let mut body = Vec::new();
        body.extend(kind.to_ne_bytes());
        body.extend(id.to_ne_bytes());
        for prop in props {
            body.extend(prop.key.to_ne_bytes());
            body.extend(prop.flags.to_ne_bytes());
            prop.value.write(&mut body);
        }
        let mut out = Vec::new();
        header(&mut out, body.len() as u32, TYPE_OBJECT);
        out.extend(body);
        pad(&mut out);
        Pod(out.chunks(8).map(|c| u64::from_ne_bytes(c.try_into().unwrap())).collect())
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr() as *const u8
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|w| w.to_ne_bytes()).collect()
    }
}

/// An object pod read back: its object type, id and properties. Properties it can't read (strings, arrays) are left out.
#[derive(Debug)]
pub struct ObjectPod {
    pub kind: u32,
    pub id: u32,
    pub props: Vec<Prop>,
}

impl ObjectPod {
    pub fn prop(&self, key: u32) -> Option<&Prop> {
        self.props.iter().find(|p| p.key == key)
    }

    /// The pod at `pod`, which libpipewire hands to a callback. Needs null or a whole valid pod for the length of the call.
    ///
    /// # Safety
    /// `pod` must be null or point at a whole valid pod that stays readable for the length of the call.
    pub unsafe fn from_ptr(pod: *const u8) -> Option<ObjectPod> {
        if pod.is_null() {
            return None;
        }
        // SAFETY: a pod starts with its 8-byte header
        let head = unsafe { std::slice::from_raw_parts(pod, 8) };
        let size = u32::from_ne_bytes(head[..4].try_into().unwrap()) as usize;
        // SAFETY: the pod is its header plus size body bytes
        ObjectPod::parse(unsafe { std::slice::from_raw_parts(pod, 8 + size) })
    }

    pub fn parse(bytes: &[u8]) -> Option<ObjectPod> {
        let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_ne_bytes(b.try_into().unwrap()));
        if u32_at(4)? != TYPE_OBJECT {
            return None;
        }
        let end = (8 + u32_at(0)? as usize).min(bytes.len());
        let (kind, id) = (u32_at(8)?, u32_at(12)?);
        let mut props = Vec::new();
        let mut at = 16;
        while at + 16 <= end {
            let (key, flags) = (u32_at(at)?, u32_at(at + 4)?);
            let (size, value_kind) = (u32_at(at + 8)? as usize, u32_at(at + 12)?);
            let body = bytes.get(at + 16..at + 16 + size)?;
            let value = if value_kind == TYPE_CHOICE {
                let choice = match u32::from_ne_bytes(body.get(..4)?.try_into().ok()?) {
                    1 => Choice::Range,
                    3 => Choice::Enum,
                    4 => Choice::Flags,
                    _ => Choice::None,
                };
                let child_size = u32::from_ne_bytes(body.get(8..12)?.try_into().ok()?) as usize;
                let child_kind = u32::from_ne_bytes(body.get(12..16)?.try_into().ok()?);
                let values: Vec<Scalar> = if child_size == 0 { Vec::new() } else { body[16..].chunks_exact(child_size).filter_map(|c| Scalar::read(child_kind, c)).collect() };
                Some(Value::Choice(choice, values))
            } else {
                Scalar::read(value_kind, body).map(Value::One)
            };
            if let Some(value) = value {
                props.push(Prop { key, flags, value });
            }
            at += 16 + size.div_ceil(8) * 8;
        }
        Some(ObjectPod { kind, id, props })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_survives_being_written_and_read() {
        let props = vec![
            Prop { key: FORMAT_MEDIA_TYPE, flags: 0, value: Value::One(Scalar::Id(MEDIA_TYPE_VIDEO)) },
            Prop { key: FORMAT_VIDEO_MODIFIER, flags: PROP_MANDATORY | PROP_DONT_FIXATE, value: Value::Choice(Choice::Enum, vec![Scalar::Long(0), Scalar::Long(0)]) },
            Prop { key: FORMAT_VIDEO_SIZE, flags: 0, value: Value::Choice(Choice::Range, vec![Scalar::Rect(640, 480), Scalar::Rect(1, 1), Scalar::Rect(16384, 16384)]) },
            Prop { key: FORMAT_VIDEO_FORMAT, flags: 0, value: Value::Choice(Choice::Enum, vec![Scalar::Id(12), Scalar::Id(12), Scalar::Id(11)]) },
        ];
        let pod = Pod::object(OBJECT_FORMAT, PARAM_ENUM_FORMAT, &props);
        let bytes = pod.bytes();
        assert_eq!(bytes.len() % 8, 0);
        let back = ObjectPod::parse(&bytes).unwrap();
        assert_eq!((back.kind, back.id), (OBJECT_FORMAT, PARAM_ENUM_FORMAT));
        assert_eq!(back.props, props);
        // SAFETY: the pointer is the whole pod made above
        let again = unsafe { ObjectPod::from_ptr(pod.as_ptr()) }.unwrap();
        assert_eq!(again.props.len(), 4);
        assert_eq!(again.prop(FORMAT_VIDEO_SIZE).unwrap().value.default_value(), Some(Scalar::Rect(640, 480)));
    }

    #[test]
    fn a_pod_cut_short_reads_as_nothing_instead_of_panicking() {
        let pod = Pod::object(OBJECT_FORMAT, PARAM_FORMAT, &[Prop { key: FORMAT_VIDEO_SIZE, flags: 0, value: Value::One(Scalar::Rect(2, 2)) }]);
        let bytes = pod.bytes();
        for cut in 0..bytes.len() {
            let _ = ObjectPod::parse(&bytes[..cut]);
        }
    }
}
