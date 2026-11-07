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
    /// A menu's choices as the camera words them, by menu entry (when the numbers say nothing).
    pub labels: Vec<String>,
    /// Not kept in the saved settings or put back by Reset: the camera holds it, and what it offers changes with its mode.
    pub transient: bool,
}

impl Control {
    pub fn new(name: &str, kind: &str, minimum: i64, maximum: i64, step: i64, default: i64, value: i64) -> Control {
        Control { name: name.into(), kind: kind.into(), minimum, maximum, step, default, value, menu: Vec::new(), inactive: false, unit: String::new(), labels: Vec::new(), transient: false }
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
        return "-".into();
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

// ── Windows: DirectShow's picture controls, asked of the Media Foundation source ─────────────────────────

const DS_AUTO: i32 = 1;
const DS_MANUAL: i32 = 2;

/// One DirectShow property: in IAMCameraControl (camera) or IAMVideoProcAmp, its id there, our name and unit, and the
/// name of the auto switch that goes with it.
pub struct DsProp {
    pub camera: bool,
    pub id: i32,
    pub name: &'static str,
    pub unit: &'static str,
    pub auto: Option<&'static str>,
}

const fn ds(camera: bool, id: i32, name: &'static str, unit: &'static str, auto: Option<&'static str>) -> DsProp {
    DsProp { camera, id, name, unit, auto }
}

pub const DS_PROPS: [DsProp; 14] = [
    ds(false, 0, "brightness", "", None), ds(false, 1, "contrast", "", None), ds(false, 2, "hue", "", None),
    ds(false, 3, "saturation", "", None), ds(false, 4, "sharpness", "", None), ds(false, 5, "gamma", "", None),
    ds(false, 7, "white_balance", "kelvin", Some("white_balance_auto")), ds(false, 8, "backlight", "", None),
    ds(false, 9, "gain", "", None),
    ds(true, 0, "pan", "arcsec", None), ds(true, 1, "tilt", "arcsec", None), ds(true, 3, "zoom", "zoom", None),
    ds(true, 4, "exposure_time", "log2s", Some("exposure_auto")), ds(true, 6, "focus", "", Some("focus_auto")),
];

/// What one DirectShow property reported: its range, the modes it can be in (caps) and where it is now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DsRead {
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub default: i32,
    pub caps: i32,
    pub value: i32,
    pub flags: i32,
}

/// The controls a property gives the card: the number itself when it can be set by hand over a real range, and its
/// auto switch when the camera can do auto.
pub fn ds_controls(prop: &DsProp, r: &DsRead) -> Vec<Control> {
    let mut out = Vec::new();
    let auto_on = r.flags & DS_AUTO != 0;
    let manual = r.caps & DS_MANUAL != 0 || r.caps & DS_AUTO == 0;
    if manual && r.max > r.min {
        let mut c = Control::new(prop.name, "int", i64::from(r.min), i64::from(r.max), i64::from(r.step.max(1)), i64::from(r.default), i64::from(r.value));
        c.unit = prop.unit.into();
        c.inactive = prop.auto.is_some() && auto_on;
        out.push(c);
    }
    if let (Some(auto), true) = (prop.auto, r.caps & DS_AUTO != 0) {
        out.push(Control::new(auto, "bool", 0, 1, 1, 1, i64::from(auto_on)));
    }
    out
}

/// The property a control name belongs to, and whether the name is its auto switch.
pub fn ds_target(name: &str) -> Option<(&'static DsProp, bool)> {
    DS_PROPS.iter().find_map(|p| if p.name == name { Some((p, false)) } else { (p.auto == Some(name)).then_some((p, true)) })
}

/// The flags to set a property with: auto on or off for a switch, manual for a number.
pub fn ds_flags(auto_switch: bool, value: i64) -> i32 {
    if auto_switch && value != 0 { DS_AUTO } else { DS_MANUAL }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::Interface;
    use windows::Win32::Media::DirectShow::{IAMCameraControl, IAMVideoProcAmp};

    enum Iface {
        Proc(IAMVideoProcAmp),
        Cam(IAMCameraControl),
    }

    impl Iface {
        fn read(&self, id: i32) -> windows::core::Result<DsRead> {
            let mut r = DsRead::default();
            // SAFETY: the pointers are fields of r, which lives through the calls
            unsafe {
                match self {
                    Iface::Proc(p) => p.GetRange(id, &mut r.min, &mut r.max, &mut r.step, &mut r.default, &mut r.caps)?,
                    Iface::Cam(c) => c.GetRange(id, &mut r.min, &mut r.max, &mut r.step, &mut r.default, &mut r.caps)?,
                }
                // A property the camera can read no value of still lists, at its default
                let got = match self {
                    Iface::Proc(p) => p.Get(id, &mut r.value, &mut r.flags),
                    Iface::Cam(c) => c.Get(id, &mut r.value, &mut r.flags),
                };
                if got.is_err() {
                    r.value = r.default;
                    r.flags = if r.caps & DS_AUTO != 0 { DS_AUTO } else { DS_MANUAL };
                }
            }
            Ok(r)
        }

        fn write(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()> {
            // SAFETY: plain COM calls on a live interface
            unsafe {
                match self {
                    Iface::Proc(p) => p.Set(id, value, flags),
                    Iface::Cam(c) => c.Set(id, value, flags),
                }
            }
        }
    }

    /// A media source kept open with the two interfaces asked of it, so a slider drag doesn't open the camera for every step.
    struct Held {
        source: windows::Win32::Media::MediaFoundation::IMFMediaSource,
        proc: Option<Iface>,
        cam: Option<Iface>,
        /// What asking for the interfaces gave, for the log.
        note: String,
    }

    // SAFETY: Media Foundation's objects are free-threaded, and a Held is only used by the one call at a time that owns the plugin
    unsafe impl Send for Held {}

    impl Drop for Held {
        fn drop(&mut self) {
            // SAFETY: shutting the source down releases the camera
            let _ = unsafe { self.source.Shutdown() };
        }
    }

    /// The controls of one camera, read and set through one media source that stays open while this is kept (which works while a
    /// stream holds the camera too) and is made again if the camera stops answering through it.
    pub struct WinControls {
        link: String,
        name: String,
        logged: String,
        held: Option<Held>,
    }

    type Listing = (Vec<Control>, Vec<String>, Vec<String>, String, bool);

    impl WinControls {
        pub fn new(link: &str, name: &str) -> WinControls {
            WinControls { link: link.into(), name: name.into(), logged: String::new(), held: None }
        }

        fn say(&mut self, line: String) {
            if self.logged != line {
                log::info!("{line}");
                self.logged = line;
            }
        }

        fn code(e: &windows::core::Error) -> String {
            format!("{:#010x}", e.code().0 as u32)
        }

        /// The source kept open, made first if there isn't one; the bool says it was already open before this call.
        fn held(&mut self) -> Result<(&Held, bool), String> {
            let before = self.held.is_some();
            if !before {
                let source = crate::webcams_win::device_source(&self.link).map_err(|e| format!("couldn't open the camera: {e}"))?;
                let proc = source.cast::<IAMVideoProcAmp>();
                let cam = source.cast::<IAMCameraControl>();
                let note = format!("IAMVideoProcAmp {}, IAMCameraControl {}",
                                   proc.as_ref().map_or_else(Self::code, |_| "ok".into()),
                                   cam.as_ref().map_or_else(Self::code, |_| "ok".into()));
                self.held = Some(Held { source, proc: proc.ok().map(Iface::Proc), cam: cam.ok().map(Iface::Cam), note });
            }
            self.held.as_ref().map(|h| (h, before)).ok_or_else(|| "no source".into())
        }

        /// One pass over the properties on the open source: what the camera offers, what it listed and refused, the log note, and
        /// whether the source was open already.
        fn read_all(&mut self) -> Result<Listing, String> {
            let (held, before) = self.held()?;
            let (mut out, mut found, mut missing) = (Vec::new(), Vec::new(), Vec::new());
            for prop in &DS_PROPS {
                let Some(iface) = (if prop.camera { held.cam.as_ref() } else { held.proc.as_ref() }) else { continue };
                match iface.read(prop.id) {
                    Ok(r) => {
                        let made = ds_controls(prop, &r);
                        found.push(format!("{} {}..{} caps {}{}", prop.name, r.min, r.max, r.caps, if made.is_empty() { " (unusable)" } else { "" }));
                        out.extend(made);
                    }
                    Err(e) => missing.push(format!("{} {}", prop.name, Self::code(&e))),
                }
            }
            Ok((out, found, missing, held.note.clone(), before))
        }

        fn write(&mut self, name: &str, value: i64) -> Result<(), String> {
            let Some((prop, auto_switch)) = ds_target(name) else { return Err("no such control".into()) };
            let (held, _) = self.held()?;
            match if prop.camera { held.cam.as_ref() } else { held.proc.as_ref() } {
                Some(iface) => {
                    // An auto switch keeps the value it has; a number goes in by hand
                    let now = if auto_switch { iface.read(prop.id).map(|r| r.value).unwrap_or(0) } else { value as i32 };
                    iface.write(prop.id, now, ds_flags(auto_switch, value)).map_err(|e| e.to_string())
                }
                None => Err("the camera has no such control".into()),
            }
        }
    }

    impl Controls for WinControls {
        fn list(&mut self) -> Vec<Control> {
            let mut got = self.read_all();
            // A source kept from before that lists nothing any more (the camera was replugged or reset) is made again
            if matches!(&got, Ok((_, found, _, _, true)) if found.is_empty()) {
                self.held = None;
                got = self.read_all();
            }
            match got {
                Ok((out, found, missing, note, _)) => {
                    self.say(format!("Webcam controls of {}: {note}; found [{}]; refused [{}]", self.name, found.join(", "), missing.join(", ")));
                    out
                }
                Err(why) => {
                    self.say(format!("Webcam controls of {}: {why}", self.name));
                    Vec::new()
                }
            }
        }

        fn set(&mut self, name: &str, value: i64) -> bool {
            if ds_target(name).is_none() {
                return false;
            }
            let mut done = self.write(name, value);
            if done.is_err() && self.held.is_some() {
                // Once more on a fresh source, in case the kept one went stale
                self.held = None;
                done = self.write(name, value);
            }
            if let Err(why) = &done {
                log::debug!("the webcam refused {name} = {value}: {why}");
            }
            done.is_ok()
        }
    }
}

/// What reads and sets a webcam's controls on this system, or None where that can't be done.
pub fn controls_for(device: &crate::webcams::Device) -> Option<Box<dyn Controls>> {
    if crate::webcams::is_gphoto(&device.location) {
        // A DSLR's settings, while its live view streams through libgphoto2
        #[cfg(target_os = "linux")]
        return Some(Box::new(crate::webcam_capture::gphoto_lib::GphotoControls::new(&device.location)));
        #[cfg(not(target_os = "linux"))]
        return None;
    }
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
    #[cfg(windows)]
    {
        Some(Box::new(win::WinControls::new(&device.location, &device.name)))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
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

    /// (kind, min, max, step, default, flags, menu values)
    type FakeCtrl = (u32, i64, i64, i64, i64, u32, Vec<i64>);

    /// Answers the control ioctls the way a webcam with brightness, auto exposure and anti-flicker does.
    #[derive(Clone)]
    pub struct FakeV4l2 {
        ctrls: HashMap<u32, FakeCtrl>,
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

    #[test]
    fn directshow_properties_become_numbers_and_auto_switches() {
        let prop = |name| DS_PROPS.iter().find(|p| p.name == name).unwrap();
        let read = |min, max, step, default, caps, value, flags| DsRead { min, max, step, default, caps, value, flags };
        // Brightness by hand only
        let got = ds_controls(prop("brightness"), &read(0, 255, 1, 128, 2, 140, 2));
        assert_eq!(got, [Control::new("brightness", "int", 0, 255, 1, 128, 140)]);
        // Focus with auto on: the number is greyed out and the switch is on
        let got = ds_controls(prop("focus"), &read(0, 250, 0, 0, 3, 40, 1));
        assert_eq!((got[0].name.as_str(), got[0].step, got[0].inactive), ("focus", 1, true));
        assert_eq!((got[1].name.as_str(), got[1].kind.as_str(), got[1].value), ("focus_auto", "bool", 1));
        // Auto-only exposure: just the switch; a flat range is no control
        let got = ds_controls(prop("exposure_time"), &read(0, 0, 0, 0, 1, 0, 1));
        assert_eq!(got.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["exposure_auto"]);
        assert!(ds_controls(prop("zoom"), &read(100, 100, 1, 100, 2, 100, 2)).is_empty());
        assert_eq!(ds_target("exposure_auto").map(|(p, a)| (p.name, a)), Some(("exposure_time", true)));
        assert_eq!(ds_target("pan").map(|(p, a)| (p.id, a)), Some((0, false)));
        assert!(ds_target("anti_flicker").is_none());
        assert_eq!((ds_flags(true, 1), ds_flags(true, 0), ds_flags(false, 5)), (DS_AUTO, DS_MANUAL, DS_MANUAL));
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
