//! A webcam's own picture settings (webcam_controls.py): exposure, white balance, focus and the like, for the Webcam
//! card. Only what the camera reports is offered, with its own range, step and default. Linux asks V4L2 directly.

use crate::webcams::{put_u32, u32_at, Ioctl};

/// Auto switches and the manual control each one greys out while it's on.
pub const AUTO_OF: [(&str, &str); 3] = [("exposure_time", "exposure_auto"), ("white_balance", "white_balance_auto"), ("focus", "focus_auto")];
pub const AUTOS: [&str; 3] = ["exposure_auto", "white_balance_auto", "focus_auto"];

pub const FLICKER_OFF: i64 = 0;
pub const FLICKER_50: i64 = 1;
pub const FLICKER_60: i64 = 2;
pub const FLICKER_AUTO: i64 = 3;
/// Where mains electricity runs at 60 Hz; everywhere else is 50 Hz.
const MAINS_60: [&str; 34] = ["US", "CA", "MX", "BR", "CO", "VE", "EC", "PE", "BO", "GT", "HN", "SV", "NI", "CR", "PA", "CU", "DO",
                              "PR", "HT", "BS", "BZ", "TT", "JM", "BB", "GU", "KR", "TW", "PH", "SA", "LR", "SR", "GY", "AS", "VI"];

pub fn auto_of(name: &str) -> Option<&'static str> {
    AUTO_OF.iter().find(|(m, _)| *m == name).map(|(_, a)| *a)
}

/// One setting a camera offers. unit says how to show the number: "" as it is, "100us" (exposure in 0.1 ms), "log2s",
/// "kelvin", "arcsec" (pan and tilt) or "zoom".
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub name: String,
    /// "int", "bool" or "menu".
    pub kind: String,
    pub minimum: i64,
    pub maximum: i64,
    pub step: i64,
    pub default: i64,
    pub value: i64,
    pub menu: Vec<i64>,
    /// The camera says it does nothing right now (its auto partner is on).
    pub inactive: bool,
    pub unit: String,
}

impl Control {
    pub fn new(name: &str, kind: &str, minimum: i64, maximum: i64, step: i64, default: i64, value: i64) -> Control {
        Control { name: name.into(), kind: kind.into(), minimum, maximum, step, default, value, menu: Vec::new(), inactive: false, unit: String::new() }
    }

    pub fn clamp(&self, value: i64) -> i64 {
        let v = value.clamp(self.minimum, self.maximum);
        if self.kind != "int" {
            return v;
        }
        let step = self.step.max(1);
        self.minimum + ((v - self.minimum) as f64 / step as f64).round_ties_even() as i64 * step
    }
}

/// 50 or 60: the mains frequency in territory (a two-letter code).
pub fn mains_hz(territory: &str) -> u32 {
    if MAINS_60.contains(&territory.to_uppercase().as_str()) { 60 } else { 50 }
}

/// Where this computer is: the time zone's country where it's a well-known one, else the language's territory.
pub fn local_territory() -> String {
    #[cfg(test)]
    if let Some(t) = tests::TERRITORY.lock().unwrap().clone() {
        return t;
    }
    const ZONES: [(&str, &str); 14] = [
        ("America/New_York", "US"), ("America/Chicago", "US"), ("America/Denver", "US"), ("America/Los_Angeles", "US"),
        ("America/Phoenix", "US"), ("America/Toronto", "CA"), ("America/Vancouver", "CA"), ("America/Mexico_City", "MX"),
        ("America/Sao_Paulo", "BR"), ("America/Bogota", "CO"), ("Asia/Seoul", "KR"), ("Asia/Taipei", "TW"),
        ("Asia/Manila", "PH"), ("Asia/Riyadh", "SA"),
    ];
    let zone = std::env::var("TZ").ok().or_else(|| {
        std::fs::read_link("/etc/localtime").ok().map(|p| p.to_string_lossy().split("zoneinfo/").nth(1).unwrap_or("").to_string())
    }).unwrap_or_default();
    if let Some((_, t)) = ZONES.iter().find(|(z, _)| *z == zone.trim_start_matches(':')) {
        return t.to_string();
    }
    if zone.starts_with("Europe/") || zone.starts_with("Africa/") || zone.starts_with("Australia/") {
        return "GB".into(); // a 50 Hz place
    }
    let lang = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty())).unwrap_or_default();
    lang.split(['.', '@']).next().unwrap_or("").split_once('_').map(|(_, t)| t.chars().take(2).collect()).unwrap_or_default()
}

/// The anti-flicker choice for the local mains, if the camera offers it.
pub fn local_flicker(control: &Control) -> Option<i64> {
    let want = if mains_hz(&local_territory()) == 60 { FLICKER_60 } else { FLICKER_50 };
    control.menu.contains(&want).then_some(want)
}

/// What Reset puts a control back to: the camera's default, but anti-flicker to the local mains.
pub fn default_of(control: &Control) -> i64 {
    if control.name == "anti_flicker" {
        if let Some(local) = local_flicker(control) {
            return local;
        }
    }
    control.default
}

/// Auto switches first, so the manual values that follow aren't refused while auto is still on.
pub fn apply_order(names: &[String]) -> Vec<String> {
    let mut out = names.to_vec();
    out.sort_by_key(|n| (!AUTOS.contains(&n.as_str()), n.clone()));
    out
}

/// An exposure time the way cameras show it: 1/60, or 0.5 s for long ones.
pub fn shutter_text(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "–".into();
    }
    if seconds >= 0.4 {
        return format!("{seconds:.1} s").replace(".0 s", " s");
    }
    format!("1/{}", (1.0 / seconds).round_ties_even() as i64)
}

/// How the card shows a control's number.
pub fn shown_value(control: &Control, value: i64) -> String {
    match control.unit.as_str() {
        "100us" => shutter_text(value as f64 / 10000.0),
        "log2s" => shutter_text(2f64.powi(value as i32)),
        "kelvin" => format!("{value} K"),
        "arcsec" => {
            let deg = (value as f64 / 3600.0).round_ties_even() as i64;
            if deg == 0 { "0°".into() } else { format!("{deg:+}°") }
        }
        "zoom" if control.minimum > 0 => format!("{:.1}×", value as f64 / control.minimum as f64),
        _ => value.to_string(),
    }
}

// ── Linux: V4L2 controls ─────────────────────────────────────────────────────

const QUERYCTRL_SIZE: usize = 68;
const QUERYMENU_SIZE: usize = 44;
pub const VIDIOC_G_CTRL: u32 = (3 << 30) | (8 << 16) | ((b'V' as u32) << 8) | 27;
pub const VIDIOC_S_CTRL: u32 = (3 << 30) | (8 << 16) | ((b'V' as u32) << 8) | 28;
pub const VIDIOC_QUERYCTRL: u32 = (3 << 30) | ((QUERYCTRL_SIZE as u32) << 16) | ((b'V' as u32) << 8) | 36;
pub const VIDIOC_QUERYMENU: u32 = (3 << 30) | ((QUERYMENU_SIZE as u32) << 16) | ((b'V' as u32) << 8) | 37;
const TYPE_INT: u32 = 1;
const TYPE_BOOL: u32 = 2;
const TYPE_MENU: u32 = 3;
const FLAG_DISABLED: u32 = 0x1;
const FLAG_READ_ONLY: u32 = 0x4;
const FLAG_INACTIVE: u32 = 0x10;
/// V4L2_EXPOSURE_MANUAL; every other choice is some kind of auto.
const EXPOSURE_MANUAL: i64 = 1;

/// Our names for the V4L2 controls the card offers, with how to show them.
pub const V4L2_CONTROLS: [(u32, &str, &str); 19] = [
    (0x0098_0900, "brightness", ""), (0x0098_0901, "contrast", ""), (0x0098_0902, "saturation", ""),
    (0x0098_0903, "hue", ""), (0x0098_090C, "white_balance_auto", ""), (0x0098_0910, "gamma", ""),
    (0x0098_0913, "gain", ""), (0x0098_0918, "anti_flicker", ""), (0x0098_091A, "white_balance", "kelvin"),
    (0x0098_091B, "sharpness", ""), (0x0098_091C, "backlight", ""),
    (0x009A_0901, "exposure_auto", ""), (0x009A_0902, "exposure_time", "100us"),
    (0x009A_0903, "frame_rate_drop", ""), (0x009A_0908, "pan", "arcsec"), (0x009A_0909, "tilt", "arcsec"),
    (0x009A_090A, "focus", ""), (0x009A_090C, "focus_auto", ""), (0x009A_090D, "zoom", "zoom"),
];

/// What reads and sets one camera's controls.
pub trait Controls: Send {
    fn list(&mut self) -> Vec<Control>;
    fn set(&mut self, name: &str, value: i64) -> bool;
}

/// Opens the node for one call (which works while a stream holds it too); None when it can't be opened.
pub type OpenFd = Box<dyn Fn() -> Option<Box<dyn Ioctl>> + Send>;

/// The V4L2 controls of one node.
pub struct LinuxControls {
    open: OpenFd,
    /// What auto exposure is on this camera: aperture priority on most webcams.
    exposure_auto: i64,
}

impl LinuxControls {
    pub fn new(open: OpenFd) -> LinuxControls {
        LinuxControls { open, exposure_auto: 3 }
    }

    fn menu(io: &dyn Ioctl, cid: u32, lo: i64, hi: i64) -> Vec<i64> {
        (lo..=hi.min(lo + 32)).filter(|i| {
            let mut b = [0u8; QUERYMENU_SIZE];
            put_u32(&mut b, 0, cid);
            put_u32(&mut b, 4, *i as u32);
            io.call(VIDIOC_QUERYMENU, &mut b)
        }).collect()
    }

    fn query(&mut self, io: &dyn Ioctl, cid: u32, name: &str, unit: &str) -> Option<Control> {
        let mut b = [0u8; QUERYCTRL_SIZE];
        put_u32(&mut b, 0, cid);
        if !io.call(VIDIOC_QUERYCTRL, &mut b) {
            return None;
        }
        let kind = u32_at(&b, 4);
        let i = |at: usize| u32_at(&b, at) as i32 as i64;
        let (lo, hi, step, default, flags) = (i(40), i(44), i(48), i(52), u32_at(&b, 56));
        if flags & (FLAG_DISABLED | FLAG_READ_ONLY) != 0 || ![TYPE_INT, TYPE_BOOL, TYPE_MENU].contains(&kind) {
            return None;
        }
        let mut c = [0u8; 8];
        put_u32(&mut c, 0, cid);
        let value = if io.call(VIDIOC_G_CTRL, &mut c) { u32_at(&c, 4) as i32 as i64 } else { default };
        let inactive = flags & FLAG_INACTIVE != 0;
        if name == "exposure_auto" {
            // A menu in V4L2; here it's a switch, as on Windows
            let menu = Self::menu(io, cid, lo, hi);
            let autos: Vec<i64> = menu.iter().copied().filter(|v| *v != EXPOSURE_MANUAL).collect();
            if autos.is_empty() || !menu.contains(&EXPOSURE_MANUAL) {
                return None;
            }
            self.exposure_auto = if autos.contains(&default) { default } else { *autos.last().unwrap() };
            return Some(Control::new(name, "bool", 0, 1, 1, i64::from(default != EXPOSURE_MANUAL), i64::from(value != EXPOSURE_MANUAL)));
        }
        let with = |mut c: Control| {
            c.inactive = inactive;
            c.unit = unit.into();
            c
        };
        if kind == TYPE_MENU {
            let menu = Self::menu(io, cid, lo, hi);
            return (!menu.is_empty()).then(|| with(Control { menu, ..Control::new(name, "menu", lo, hi, 1, default, value) }));
        }
        if kind == TYPE_BOOL {
            return Some(with(Control::new(name, "bool", 0, 1, 1, default, value)));
        }
        if hi <= lo {
            return None;
        }
        Some(with(Control::new(name, "int", lo, hi, step.max(1), default, value)))
    }
}

impl Controls for LinuxControls {
    fn list(&mut self) -> Vec<Control> {
        let Some(io) = (self.open)() else { return Vec::new() };
        V4L2_CONTROLS.iter().filter_map(|(cid, name, unit)| self.query(io.as_ref(), *cid, name, unit)).collect()
    }

    fn set(&mut self, name: &str, value: i64) -> bool {
        let Some((cid, _, _)) = V4L2_CONTROLS.iter().find(|(_, n, _)| *n == name) else { return false };
        let value = if name == "exposure_auto" { if value != 0 { self.exposure_auto } else { EXPOSURE_MANUAL } } else { value };
        let Some(io) = (self.open)() else { return false };
        let mut c = [0u8; 8];
        put_u32(&mut c, 0, *cid);
        put_u32(&mut c, 4, value as i32 as u32);
        let ok = io.call(VIDIOC_S_CTRL, &mut c);
        if !ok {
            log::debug!("the webcam refused {name} = {value}");
        }
        ok
    }
}

/// What reads and sets a webcam's controls on this system, or None where that can't be done.
pub fn controls_for(device: &crate::webcams::Device) -> Option<Box<dyn Controls>> {
    #[cfg(target_os = "linux")]
    {
        struct Owned(std::fs::File);
        impl Ioctl for Owned {
            fn call(&self, request: u32, buf: &mut [u8]) -> bool {
                crate::webcam_capture::Fd(&self.0).call(request, buf)
            }
        }
        let path = device.location.clone();
        Some(Box::new(LinuxControls::new(Box::new(move || {
            use std::os::unix::fs::OpenOptionsExt;
            let f = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(&path).ok()?;
            Some(Box::new(Owned(f)) as Box<dyn Ioctl>)
        }))))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Where tests say the computer is.
    pub static TERRITORY: Mutex<Option<String>> = Mutex::new(None);

    /// Answers the control ioctls the way a webcam with brightness, auto exposure and anti-flicker does.
    #[derive(Clone)]
    pub struct FakeV4l2 {
        ctrls: HashMap<u32, (u32, i64, i64, i64, i64, u32, Vec<i64>)>,
        pub values: Arc<Mutex<HashMap<u32, i64>>>,
    }

    impl FakeV4l2 {
        pub fn new() -> FakeV4l2 {
            let ctrls = HashMap::from([
                (0x0098_0900, (1, 0, 255, 1, 128, 0, vec![])), (0x009A_0901, (3, 0, 3, 1, 3, 0, vec![1, 3])),
                (0x009A_0902, (1, 3, 2047, 1, 250, 0x10, vec![])), (0x0098_0918, (3, 0, 2, 1, 2, 0, vec![0, 1, 2])),
                (0x0098_091C, (1, 0, 1, 1, 0, 0x4, vec![])),
            ]);
            let values = HashMap::from([(0x0098_0900, 140), (0x009A_0901, 3), (0x009A_0902, 333), (0x0098_0918, 2), (0x0098_091C, 0)]);
            FakeV4l2 { ctrls, values: Arc::new(Mutex::new(values)) }
        }
        pub fn open(&self) -> OpenFd {
            let me = self.clone();
            Box::new(move || Some(Box::new(me.clone()) as Box<dyn Ioctl>))
        }
    }

    impl Ioctl for FakeV4l2 {
        fn call(&self, request: u32, b: &mut [u8]) -> bool {
            let cid = u32_at(b, 0);
            match request {
                VIDIOC_QUERYCTRL => {
                    let Some((kind, lo, hi, step, default, flags, _)) = self.ctrls.get(&cid) else { return false };
                    put_u32(b, 4, *kind);
                    for (at, v) in [(40, lo), (44, hi), (48, step), (52, default)] {
                        put_u32(b, at, *v as i32 as u32);
                    }
                    put_u32(b, 56, *flags);
                    true
                }
                VIDIOC_QUERYMENU => self.ctrls[&cid].6.contains(&(u32_at(b, 4) as i64)),
                VIDIOC_G_CTRL => {
                    put_u32(b, 4, self.values.lock().unwrap()[&cid] as u32);
                    true
                }
                VIDIOC_S_CTRL => {
                    self.values.lock().unwrap().insert(cid, u32_at(b, 4) as i32 as i64);
                    true
                }
                _ => false,
            }
        }
    }

    // test_webcam_controls.py::test_v4l2_controls_are_read_under_our_names_and_auto_exposure_is_a_switch
    #[test]
    fn v4l2_controls_are_read_under_our_names_and_auto_exposure_is_a_switch() {
        let fake = FakeV4l2::new();
        let got: HashMap<String, Control> = LinuxControls::new(fake.open()).list().into_iter().map(|c| (c.name.clone(), c)).collect();
        let mut names: Vec<&String> = got.keys().collect();
        names.sort();
        assert_eq!(names, ["anti_flicker", "brightness", "exposure_auto", "exposure_time"]);
        assert_eq!(got["brightness"], Control::new("brightness", "int", 0, 255, 1, 128, 140));
        let ea = &got["exposure_auto"];
        assert_eq!((ea.kind.as_str(), ea.value, ea.default), ("bool", 1, 1));
        assert!(got["exposure_time"].inactive && got["exposure_time"].unit == "100us");
        assert_eq!(got["anti_flicker"].menu, [0, 1, 2]);
    }

    // test_webcam_controls.py::test_setting_auto_exposure_writes_the_cameras_own_auto_mode_or_manual
    #[test]
    fn setting_auto_exposure_writes_the_cameras_own_auto_mode_or_manual() {
        let fake = FakeV4l2::new();
        let mut c = LinuxControls::new(fake.open());
        c.list();
        assert!(c.set("exposure_auto", 0) && fake.values.lock().unwrap()[&0x009A_0901] == 1);
        assert!(c.set("exposure_auto", 1) && fake.values.lock().unwrap()[&0x009A_0901] == 3);
        assert!(!c.set("not_a_control", 1));
    }

    // test_webcam_controls.py::test_mains_frequency_comes_from_where_the_computer_is
    #[test]
    fn mains_frequency_comes_from_where_the_computer_is() {
        assert!(mains_hz("US") == 60 && mains_hz("se") == 50 && mains_hz("BR") == 60);
    }

    // test_webcam_controls.py::test_auto_switches_are_applied_before_the_values_they_grey_out
    #[test]
    fn auto_switches_are_applied_before_the_values_they_grey_out() {
        assert_eq!(apply_order(&["exposure_time".into(), "gain".into(), "exposure_auto".into()])[0], "exposure_auto");
    }

    // test_webcam_controls.py::test_values_are_kept_to_the_cameras_range_and_step
    #[test]
    fn values_are_kept_to_the_cameras_range_and_step() {
        let focus = Control::new("focus", "int", 0, 250, 5, 0, 0);
        assert_eq!((focus.clamp(-3), focus.clamp(12), focus.clamp(999)), (0, 10, 250));
    }

    // test_webcam_controls.py::test_exposure_shows_as_a_shutter_speed
    #[test]
    fn exposure_shows_as_a_shutter_speed() {
        assert_eq!((shutter_text(1.0 / 60.0), shutter_text(0.5), shutter_text(2.0)), ("1/60".into(), "0.5 s".into(), "2 s".into()));
        let unit = |c: Control, u: &str| Control { unit: u.into(), ..c };
        assert_eq!(shown_value(&unit(Control::new("exposure_time", "int", 3, 2047, 1, 166, 166), "100us"), 166), "1/60");
        assert_eq!(shown_value(&unit(Control::new("exposure_time", "int", -13, -1, 1, -6, -6), "log2s"), -6), "1/64");
        assert_eq!(shown_value(&unit(Control::new("pan", "int", -36000, 36000, 3600, 0, 0), "arcsec"), 7200), "+2°");
        assert_eq!(shown_value(&unit(Control::new("zoom", "int", 100, 500, 1, 100, 200), "zoom"), 200), "2.0×");
    }
}
