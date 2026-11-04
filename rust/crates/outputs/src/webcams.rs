//! Webcams plugged into this computer, for the Webcam plugin (webcams.py): finding them, picking how to capture and
//! which ones are virtual. Linux finds them in sysfs and asks each V4L2 node what it is and which formats it has.
//! Telescope's own virtual cameras and other loopback or virtual ones are never listed, so a stream can't feed back
//! into itself.

use std::collections::HashMap;

pub const SYSFS: &str = "/sys/class/video4linux";
pub const BY_ID: &str = "/dev/v4l/by-id";
pub const BY_PATH: &str = "/dev/v4l/by-path";
/// Lists loopback cameras that aren't Telescope's too, for trying the Webcam plugin with fake cameras.
pub const ALLOW_LOOPBACK_ENV: &str = "TELESCOPE_WEBCAM_LOOPBACK";

/// Auto takes the largest size up to 1080p that keeps the frame rate.
pub const AUTO_LONGEST: u32 = 1920;
pub const COMPRESSED: [&str; 2] = ["MJPG", "JPEG"];
pub const RAW: [&str; 8] = ["YUYV", "UYVY", "NV12", "YU12", "YV12", "BGR3", "RGB3", "GREY"];
/// Rates offered where a camera gives a range rather than a list.
pub const FPS_STEPS: [u32; 9] = [5, 10, 15, 20, 24, 25, 30, 50, 60];
/// Same, for sizes.
pub const STEP_SIZES: [(u32, u32); 5] = [(640, 480), (1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];
/// A size is only offered when some format reaches this rate there, so slow raw-only sizes don't lag the stream.
pub const MIN_USABLE_FPS: f64 = 15.0;
/// 29.97 counts as 30.
const FPS_SLACK: f64 = 0.5;
/// Linux drivers that make virtual cameras rather than talk to hardware.
pub const VIRTUAL_DRIVERS: [&str; 2] = ["v4l2 loopback", "akvcam"];
const UC_OLD_NAME: &str = "Unity Video Capture";

/// One way a camera can send video: a pixel format at a size, with the frame rates it offers there.
#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    pub fourcc: String,
    pub width: u32,
    pub height: u32,
    pub fps: Vec<f64>,
}

impl Mode {
    pub fn new(fourcc: &str, width: u32, height: u32, fps: &[f64]) -> Mode {
        Mode { fourcc: fourcc.into(), width, height, fps: fps.to_vec() }
    }
    pub fn compressed(&self) -> bool {
        COMPRESSED.contains(&self.fourcc.as_str())
    }
    pub fn max_fps(&self) -> f64 {
        self.fps.iter().copied().fold(0.0, f64::max)
    }
}

/// What to ask a camera for.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub fourcc: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

/// Python's "%g" for the rates cameras give (30, 29.97).
pub fn fmt_g(v: f64) -> String {
    let s = format!("{:.6}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Choice {
    pub fn compressed(&self) -> bool {
        COMPRESSED.contains(&self.fourcc.as_str())
    }
    pub fn describe(&self) -> String {
        let kind = if self.compressed() { "MJPEG".to_string() } else if self.fourcc == "RAW" { String::new() } else { self.fourcc.clone() };
        let rate = if self.fps != 0.0 { format!("{} fps", fmt_g(self.fps)) } else { String::new() };
        [format!("{}×{}", self.width, self.height), kind, rate].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ")
    }
}

/// A webcam as it's plugged in now. key stays the same across replugging; location is what's opened.
#[derive(Clone, Debug)]
pub struct Device {
    pub key: String,
    pub name: String,
    /// Linux: its /dev/video node; Windows: its device path.
    pub location: String,
    pub modes: Vec<Mode>,
}

impl PartialEq for Device {
    fn eq(&self, other: &Device) -> bool {
        (&self.key, &self.name, &self.location) == (&other.key, &other.name, &other.location)
    }
}

// ── Choosing a mode ──────────────────────────────────────────────────────────

/// Every size worth offering, largest first: one some format sends at MIN_USABLE_FPS or more. When none does, every size
/// stays, so a slow camera still has a list. A mode that lists no rates counts as fast enough.
pub fn sizes(modes: &[Mode]) -> Vec<(u32, u32)> {
    let mut found: Vec<(u32, u32)> = modes.iter().map(|m| (m.width, m.height)).collect();
    found.sort_by_key(|s| std::cmp::Reverse((s.0 as u64 * s.1 as u64, s.0)));
    found.dedup();
    let fast = |s: &(u32, u32)| modes.iter().any(|m| (m.width, m.height) == *s && (m.fps.is_empty() || m.max_fps() + FPS_SLACK >= MIN_USABLE_FPS));
    if found.iter().any(fast) {
        found.retain(fast);
    }
    found
}

/// The frame rates offered at size (or at any size), rounded, highest first.
pub fn rates(modes: &[Mode], size: Option<(u32, u32)>) -> Vec<u32> {
    let mut found: Vec<u32> = modes.iter().filter(|m| size.is_none_or(|s| (m.width, m.height) == s))
        .flat_map(|m| m.fps.iter().filter(|f| **f > 0.0).map(|f| f.round_ties_even() as u32)).collect();
    found.sort_unstable_by(|a, b| b.cmp(a));
    found.dedup();
    found
}

fn auto_size(modes: &[Mode], fps: f64) -> Option<(u32, u32)> {
    let all = sizes(modes);
    let mut fitting: Vec<(u32, u32)> = all.iter().copied().filter(|s| s.0.max(s.1) <= AUTO_LONGEST).collect();
    if fitting.is_empty() {
        fitting = all.last().copied().into_iter().collect();
    }
    let at = |s: (u32, u32)| modes.iter().filter(move |m| (m.width, m.height) == s);
    for &size in &fitting {
        if at(size).any(|m| m.max_fps() + FPS_SLACK >= fps) {
            return Some(size);
        }
    }
    fitting.iter().copied().fold(None, |best: Option<((u32, u32), f64)>, s| {
        let f = at(s).map(Mode::max_fps).fold(0.0, f64::max);
        if best.is_none_or(|b| f > b.1) { Some((s, f)) } else { best }
    }).map(|b| b.0)
}

fn nearest_size(modes: &[Mode], want: (u32, u32)) -> Option<(u32, u32)> {
    let have = sizes(modes);
    if have.contains(&want) {
        return Some(want);
    }
    let bigger: Vec<(u32, u32)> = have.iter().copied().filter(|s| s.0 >= want.0 && s.1 >= want.1).collect();
    bigger.last().copied().or(have.first().copied())
}

/// What to ask for: size (None for Auto) at fps. Raw when it reaches the rate there, since it costs nothing to decode;
/// MJPEG when raw can't (USB 2 can't carry raw 1080p at 30 fps), and failing both the fastest there is.
pub fn choose(modes: &[Mode], size: Option<(u32, u32)>, fps: f64) -> Option<Choice> {
    let modes: Vec<Mode> = modes.iter().filter(|m| COMPRESSED.contains(&m.fourcc.as_str()) || RAW.contains(&m.fourcc.as_str()) || m.fourcc == "RAW").cloned().collect();
    if modes.is_empty() {
        return None;
    }
    let size = match size {
        None => auto_size(&modes, fps),
        Some(s) => nearest_size(&modes, s),
    }?;
    let here: Vec<&Mode> = modes.iter().filter(|m| (m.width, m.height) == size).collect();
    let reaches = |m: &Mode| m.fps.is_empty() || m.max_fps() + FPS_SLACK >= fps;
    let best = here.iter().find(|m| !m.compressed() && reaches(m))
        .or_else(|| here.iter().find(|m| m.compressed() && reaches(m)))
        .copied()
        .or_else(|| here.iter().copied().fold(None, |b: Option<&Mode>, m| {
            if b.is_none_or(|b| (m.max_fps(), m.compressed()) > (b.max_fps(), b.compressed())) { Some(m) } else { b }
        }))?;
    let mut offered = best.fps.clone();
    offered.sort_by(f64::total_cmp);
    let rate = offered.iter().copied().find(|f| f + FPS_SLACK >= fps).or(offered.last().copied()).unwrap_or(fps);
    Some(Choice { fourcc: best.fourcc.clone(), width: best.width, height: best.height, fps: (rate * 100.0).round() / 100.0 })
}

// ── Which devices are cameras of our own ─────────────────────────────────────

/// What Telescope's own virtual cameras are called on this system.
pub fn own_names(linux: bool) -> Vec<String> {
    let mut names: Vec<String> = (0..crate::MAX_SLOTS).map(|s| {
        if linux { format!("Phone Camera{}", if s == 0 { String::new() } else { format!(" {}", s + 1) }) } else { crate::wincam::slot_label(s) }
    }).collect();
    if !linux {
        names.push(UC_OLD_NAME.into());
    }
    names
}

/// Whether a camera is a virtual one rather than hardware: Telescope's own always, others where they say so.
pub fn is_virtual_on(name: &str, driver: &str, linux: bool) -> bool {
    if own_names(linux).iter().any(|n| n == name) || VIRTUAL_DRIVERS.contains(&driver) {
        return true;
    }
    let lower = name.to_lowercase();
    lower.contains("virtual") || lower.contains("vcam")
}

pub fn is_virtual(name: &str, driver: &str) -> bool {
    is_virtual_on(name, driver, cfg!(target_os = "linux"))
}

/// A V4L2 card name without the cut-off repeat some cameras add ("HP 320 FHD Webcam: HP 320 FHD W").
pub fn tidy_name(name: &str) -> String {
    let name = name.trim();
    if let Some((head, tail)) = name.split_once(": ") {
        if !tail.trim().is_empty() && head.starts_with(tail.trim()) {
            return head.to_string();
        }
    }
    name.to_string()
}

/// Two cameras of one model get "(2)" and on after the first, in the order they're listed.
pub fn tell_apart(devices: Vec<Device>) -> Vec<Device> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    devices.into_iter().map(|d| {
        let n = seen.entry(d.name.clone()).or_insert(0);
        *n += 1;
        let name = if *n == 1 { d.name.clone() } else { format!("{} ({})", d.name, n) };
        Device { name, ..d }
    }).collect()
}

// ── Linux: V4L2 ──────────────────────────────────────────────────────────────

const fn iowr(nr: u32, size: u32, read_only: bool) -> u32 {
    ((if read_only { 2 } else { 3 }) << 30) | (size << 16) | ((b'V' as u32) << 8) | nr
}

pub const CAP_SIZE: usize = 104;
pub const FMTDESC_SIZE: usize = 64;
pub const FRMSIZE_SIZE: usize = 44;
pub const FRMIVAL_SIZE: usize = 52;
pub const VIDIOC_QUERYCAP: u32 = iowr(0, CAP_SIZE as u32, true);
pub const VIDIOC_ENUM_FMT: u32 = iowr(2, FMTDESC_SIZE as u32, false);
pub const VIDIOC_ENUM_FRAMESIZES: u32 = iowr(74, FRMSIZE_SIZE as u32, false);
pub const VIDIOC_ENUM_FRAMEINTERVALS: u32 = iowr(75, FRMIVAL_SIZE as u32, false);
const CAP_VIDEO_CAPTURE: u32 = 0x1;
const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const DISCRETE: u32 = 1;

/// An ioctl on an open node: fills buf in place, false when the driver refused.
pub trait Ioctl {
    fn call(&self, request: u32, buf: &mut [u8]) -> bool;
}

pub fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_ne_bytes());
}

fn text(raw: &[u8]) -> String {
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
}

pub fn fourcc(code: u32) -> String {
    String::from_utf8_lossy(&code.to_le_bytes()).trim().to_string()
}

pub fn code(fourcc: &str) -> u32 {
    let mut b = [b' '; 4];
    for (i, c) in fourcc.bytes().take(4).enumerate() {
        b[i] = c;
    }
    u32::from_le_bytes(b)
}

/// (driver, card name, whether it captures video) of an open V4L2 node.
pub fn query_caps(io: &dyn Ioctl) -> Option<(String, String, bool)> {
    let mut b = [0u8; CAP_SIZE];
    if !io.call(VIDIOC_QUERYCAP, &mut b) {
        return None;
    }
    let (caps, device_caps) = (u32_at(&b, 84), u32_at(&b, 88));
    let caps = if caps & CAP_DEVICE_CAPS != 0 { device_caps } else { caps };
    Some((text(&b[0..16]), text(&b[16..48]), caps & CAP_VIDEO_CAPTURE != 0))
}

fn frame_rates(io: &dyn Ioctl, code: u32, width: u32, height: u32) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::new();
    for index in 0..64 {
        let mut b = [0u8; FRMIVAL_SIZE];
        for (i, v) in [index, code, width, height].iter().enumerate() {
            put_u32(&mut b, i * 4, *v);
        }
        if !io.call(VIDIOC_ENUM_FRAMEINTERVALS, &mut b) {
            break;
        }
        let kind = u32_at(&b, 16);
        let u: Vec<u32> = (0..6).map(|i| u32_at(&b, 20 + i * 4)).collect();
        if kind == DISCRETE {
            if u[0] != 0 {
                out.push((u[1] as f64 / u[0] as f64 * 100.0).round() / 100.0);
            }
            continue;
        }
        // A range: from its shortest interval (fastest) to its longest
        let fastest = if u[0] != 0 { u[1] as f64 / u[0] as f64 } else { 0.0 };
        let slowest = if u[2] != 0 { u[3] as f64 / u[2] as f64 } else { 0.0 };
        out.extend(FPS_STEPS.iter().map(|f| *f as f64).filter(|f| slowest - FPS_SLACK <= *f && *f <= fastest + FPS_SLACK));
        break;
    }
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// Every Mode an open V4L2 node offers in a format we read.
pub fn list_modes(io: &dyn Ioctl) -> Vec<Mode> {
    let mut modes = Vec::new();
    for index in 0..32 {
        let mut b = [0u8; FMTDESC_SIZE];
        put_u32(&mut b, 0, index);
        put_u32(&mut b, 4, BUF_TYPE_VIDEO_CAPTURE);
        if !io.call(VIDIOC_ENUM_FMT, &mut b) {
            break;
        }
        let code = u32_at(&b, 44);
        let fcc = fourcc(code);
        if !COMPRESSED.contains(&fcc.as_str()) && !RAW.contains(&fcc.as_str()) {
            continue;
        }
        for s in 0..64 {
            let mut f = [0u8; FRMSIZE_SIZE];
            put_u32(&mut f, 0, s);
            put_u32(&mut f, 4, code);
            if !io.call(VIDIOC_ENUM_FRAMESIZES, &mut f) {
                break;
            }
            let kind = u32_at(&f, 8);
            let u: Vec<u32> = (0..6).map(|i| u32_at(&f, 12 + i * 4)).collect();
            let found: Vec<(u32, u32)> = if kind == DISCRETE {
                vec![(u[0], u[1])]
            } else {
                STEP_SIZES.iter().copied().filter(|(w, h)| u[0] <= *w && *w <= u[1] && u[3] <= *h && *h <= u[4]).collect()
            };
            for (w, h) in found {
                modes.push(Mode { fourcc: fcc.clone(), width: w, height: h, fps: frame_rates(io, code, w, h) });
            }
            if kind != DISCRETE {
                break;
            }
        }
    }
    modes
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// /dev/videoN: the name udev gives it under /dev/v4l, which stays the same when it's plugged in again.
pub fn stable_keys() -> HashMap<String, String> {
    let mut keys = HashMap::new();
    for folder in [BY_PATH, BY_ID] {
        // by-id last, so it wins: it follows the camera to another port
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for e in entries.flatten() {
            let Ok(target) = std::fs::canonicalize(e.path()) else { continue };
            let name = e.file_name().to_string_lossy().to_string();
            let name = name.strip_prefix("usb-").unwrap_or(&name).to_string();
            let name = match name.rfind("-video-index") {
                Some(i) if name[i + 12..].chars().all(|c| c.is_ascii_digit()) && i + 12 < name.len() => name[..i].to_string(),
                _ => name,
            };
            keys.insert(target.to_string_lossy().to_string(), name);
        }
    }
    keys
}

/// A key from sysfs for a USB camera: its IDs, serial and interface, or its port when it has no serial ("" if it isn't
/// USB). devices is /sys/devices.
pub fn usb_key(folder: &std::path::Path, devices: &std::path::Path) -> String {
    let Ok(iface) = std::fs::canonicalize(folder.join("device")) else { return String::new() };
    let Some(usb) = iface.parent() else { return String::new() };
    let (vendor, product) = (read(&usb.join("idVendor")), read(&usb.join("idProduct")));
    if vendor.is_empty() || product.is_empty() {
        return String::new();
    }
    let serial = read(&usb.join("serial"));
    if !serial.is_empty() {
        // udev's by-id link can't tell two interfaces of one camera apart, so the interface goes in too
        let num = read(&iface.join("bInterfaceNumber"));
        return format!("usb-{vendor}:{product}-{serial}-if{}", if num.is_empty() { "00" } else { &num });
    }
    // Identical cameras without a serial share a by-id name, so the port is what tells them apart
    let devices = std::fs::canonicalize(devices).unwrap_or_else(|_| devices.to_path_buf());
    let rel = iface.strip_prefix(&devices).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| iface.to_string_lossy().to_string());
    format!("usb-port-{}", rel.replace('/', "-"))
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(not(unix))]
fn inode(_meta: &std::fs::Metadata) -> u64 {
    0
}

/// Opens a node and asks it (caps, modes); None if it can't be opened.
pub type OpenNode = Box<dyn Fn(&str) -> Option<NodeInfo> + Send>;

/// What tells a node apart from itself later: (real sysfs path, inode, the dev file's text).
type NodeIdent = (String, u64, String);

/// Lists webcams from sysfs, opening each node only when it first shows up (opening wakes a sleeping camera).
pub struct LinuxScanner {
    pub sysfs: std::path::PathBuf,
    pub dev: std::path::PathBuf,
    pub devices: std::path::PathBuf,
    pub allow_loopback: bool,
    pub open: OpenNode,
    pub stable_keys: Box<dyn Fn() -> HashMap<String, String> + Send>,
    known: HashMap<String, (NodeIdent, Option<Device>)>,
}

impl LinuxScanner {
    pub fn new(sysfs: &str, dev: &str, allow_loopback: Option<bool>, open: OpenNode, keys: Box<dyn Fn() -> HashMap<String, String> + Send>) -> LinuxScanner {
        let allow = allow_loopback.unwrap_or_else(|| std::env::var(ALLOW_LOOPBACK_ENV).as_deref() == Ok("1"));
        LinuxScanner { sysfs: sysfs.into(), dev: dev.into(), devices: "/sys/devices".into(), allow_loopback: allow, open, stable_keys: keys, known: HashMap::new() }
    }

    fn key(&self, folder: &std::path::Path, path: &str, node: &str, keys: &HashMap<String, String>) -> String {
        let usb = usb_key(folder, &self.devices);
        if !usb.is_empty() {
            return usb;
        }
        let real = std::fs::canonicalize(path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string());
        keys.get(&real).cloned().unwrap_or_else(|| node.to_string())
    }

    pub fn scan(&mut self) -> Vec<Device> {
        let mut nodes: Vec<String> = std::fs::read_dir(&self.sysfs).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("video")).collect()).unwrap_or_default();
        nodes.sort_by_key(|n| n[5..].parse::<u64>().unwrap_or(0));
        let mut keys: Option<HashMap<String, String>> = None;
        let mut known = HashMap::new();
        let mut found = Vec::new();
        for node in nodes {
            let folder = self.sysfs.join(&node);
            let name = read(&folder.join("name"));
            // v4l2loopback's own sysfs files tell its devices apart without opening them
            let loopback = folder.join("max_openers").exists();
            if is_virtual(&name, "") || (loopback && !self.allow_loopback) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&folder) else { continue };
            let real = std::fs::canonicalize(&folder).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let ident = (real, inode(&meta), read(&folder.join("dev")));
            if let Some(cached) = self.known.get(&node).filter(|c| c.0 == ident && !loopback).cloned() {
                let mut cached = cached;
                if let Some(dev) = cached.1.as_mut().filter(|d| d.key == node) {
                    // udev hadn't made its link yet: look again
                    let k = keys.get_or_insert_with(|| (self.stable_keys)());
                    dev.key = self.key(&folder, &dev.location, &node, k);
                }
                if let Some(d) = &cached.1 {
                    found.push(d.clone());
                }
                known.insert(node, cached);
                continue;
            }
            let path = self.dev.join(&node).to_string_lossy().to_string();
            let Some(opened) = (self.open)(&path) else { continue }; // no permission or it went away: try again later
            let mut device = None;
            if let (Some((driver, card, captures)), modes) = opened {
                let virtual_ = VIRTUAL_DRIVERS.contains(&driver.as_str()) && !(self.allow_loopback && driver == "v4l2 loopback");
                let shown = if card.is_empty() { name.clone() } else { card.clone() };
                if captures && !modes.is_empty() && !virtual_ && !is_virtual(&shown, "") {
                    let k = keys.get_or_insert_with(|| (self.stable_keys)());
                    device = Some(Device { key: self.key(&folder, &path, &node, k), name: tidy_name(&shown), location: path, modes });
                }
            }
            if let Some(d) = &device {
                found.push(d.clone());
            }
            known.insert(node, (ident, device));
        }
        self.known = known;
        tell_apart(found)
    }
}

/// What a node says about itself: (driver, card, can capture) when it answers, and its modes.
pub type NodeInfo = (Option<(String, String, bool)>, Vec<Mode>);

/// Opens a V4L2 node for a moment and asks it what it is.
#[cfg(target_os = "linux")]
pub fn open_node(path: &str) -> Option<NodeInfo> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(path).ok()?;
    let io = crate::webcam_capture::Fd(&file);
    let caps = query_caps(&io);
    let modes = if caps.as_ref().is_some_and(|c| c.2) { list_modes(&io) } else { Vec::new() };
    Some((caps, modes))
}

/// The scanner as the plugin uses it.
#[cfg(target_os = "linux")]
pub fn linux_scanner() -> LinuxScanner {
    LinuxScanner::new(SYSFS, "/dev", None, Box::new(open_node), Box::new(stable_keys))
}

// ── Who has a camera open ────────────────────────────────────────────────────

/// Linux: the name of another program that has path open, or "" (only programs of this user can be seen).
pub fn who_uses(path: &str, proc_root: &str) -> String {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    let own = std::process::id().to_string();
    let Ok(entries) = std::fs::read_dir(proc_root) else { return String::new() };
    for e in entries.flatten() {
        let pid = e.file_name().to_string_lossy().to_string();
        if !pid.chars().all(|c| c.is_ascii_digit()) || pid == own {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|l| l == target) {
                return read(&e.path().join("comm"));
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests;
