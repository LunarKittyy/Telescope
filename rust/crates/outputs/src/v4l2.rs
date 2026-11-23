//! The Linux virtual camera: v4l2loopback as an output device, fed packed I420 with plain write() calls (what
//! pyvirtualcam does for vcam.py, minus the BGR to I420 conversion the Python app needs first), and the slots' nodes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use telescope_core::i18n::{tr, Fields};

const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_PIX_FMT_YUV420: u32 = u32::from_le_bytes(*b"YU12");
const V4L2_PIX_FMT_BGR24: u32 = u32::from_le_bytes(*b"BGR3");
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_COLORSPACE_SRGB: u32 = 8;
const VIDIOC_S_FMT: libc::c_ulong = 0xc0d0_5605; // _IOWR('V', 5, struct v4l2_format), 208 bytes
const VIDIOC_S_PARM: libc::c_ulong = 0xc0cc_5616; // _IOWR('V', 22, struct v4l2_streamparm), 204 bytes

/// Where the kernel lists video devices; tests point it elsewhere.
pub const SYSFS: &str = "/sys/class/video4linux";
pub use telescope_core::output::V4L2_PHONE_DEV as PHONE_DEV;
/// The other node the setup makes, for OBS.
pub const OBS_DEV: &str = "/dev/video10";

#[repr(C)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct Format {
    type_: u32,
    _pad: u32,
    pix: PixFormat,
    _rest: [u8; 200 - std::mem::size_of::<PixFormat>()],
}

#[repr(C)]
struct StreamParm {
    type_: u32,
    capability: u32,
    outputmode: u32,
    numerator: u32,
    denominator: u32,
    extendedmode: u32,
    writebuffers: u32,
    _rest: [u8; 200 - 24],
}

pub struct VirtualCamera {
    file: File,
    pub device: String,
    pub width: u32,
    pub height: u32,
}

/// _vcam_format: I420 needs even sizes, so an odd one goes in BGR.
pub fn takes_bgr(width: u32, height: u32) -> bool {
    !width.is_multiple_of(2) || !height.is_multiple_of(2)
}

/// (pixel format, bytes per line, bytes per frame) a camera of this size opens with.
pub fn wire_format(width: u32, height: u32) -> (u32, u32, u32) {
    if takes_bgr(width, height) {
        (V4L2_PIX_FMT_BGR24, width * 3, width * height * 3)
    } else {
        (V4L2_PIX_FMT_YUV420, width, width * height * 3 / 2)
    }
}

pub use crate::linux_setup::slot_label;

/// The first device under sysfs whose name is label, else None.
pub fn find_by_label_in(sysfs: &str, label: &str) -> Option<String> {
    let mut found: Vec<_> = std::fs::read_dir(sysfs).ok()?.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("video")).collect();
    found.sort_by_key(|e| e.file_name());
    found.into_iter().find_map(|e| {
        let name = std::fs::read_to_string(e.path().join("name")).ok()?;
        (name.trim() == label).then(|| format!("/dev/{}", e.file_name().to_string_lossy()))
    })
}

/// The first v4l2loopback device whose name is label, else None.
pub fn find_by_label(label: &str) -> Option<String> {
    find_by_label_in(SYSFS, label)
}

/// Whether the usual node's name says it is another app's loopback (an unreadable name doesn't count against it).
fn usual_node_is_foreign(sysfs: &str) -> bool {
    let Some(node) = std::path::Path::new(PHONE_DEV).file_name() else { return false };
    match std::fs::read_to_string(format!("{sysfs}/{}/name", node.to_string_lossy())) {
        Ok(name) => !crate::linux_setup::slot_labels(0).iter().any(|l| *l == name.trim()),
        Err(_) => false,
    }
}

/// The usual node of the main camera unless it's missing or another app's loopback holds it (an unreadable name doesn't count against).
pub fn phone_dev_in(sysfs: &str) -> Option<String> {
    let node = std::path::Path::new(PHONE_DEV).file_name()?.to_string_lossy().to_string();
    if !std::path::Path::new(&format!("{sysfs}/{node}")).exists() || usual_node_is_foreign(sysfs) {
        return None;
    }
    Some(PHONE_DEV.into())
}

/// slot's /dev/video node, or None if it isn't set up (vcam.slot_device). The extra ones took whatever number was
/// free, so they're found by their label, the current one first and then the one older setups still have.
pub fn slot_device_in(sysfs: &str, slot: u32) -> Option<String> {
    if slot == 0 {
        return phone_dev_in(sysfs).or_else(|| crate::linux_setup::slot_labels(0).iter().find_map(|label| find_by_label_in(sysfs, label)));
    }
    crate::linux_setup::slot_labels(slot).iter().find_map(|label| find_by_label_in(sysfs, label))
}

/// Whether slot's camera is set up to stream to (the first one is whenever Telescope is).
pub fn slot_ready_in(sysfs: &str, slot: u32) -> bool {
    slot == 0 || slot_device_in(sysfs, slot).is_some()
}

/// Whether the main camera is there: at the usual node (usual_node_exists) unless that is another app's loopback, or hand-made under another number and found by its label.
pub fn main_camera_ready_in(sysfs: &str, usual_node_exists: bool) -> bool {
    (usual_node_exists && !usual_node_is_foreign(sysfs)) || slot_device_in(sysfs, 0).is_some()
}

/// The size device is held at while a reader still has buffers ("YU12:1280x720@30" in sysfs), else None.
/// v4l2loopback silently ignores a new writer asking for another size, so the next writer has to use this one.
pub fn locked_size_in(sysfs: &str, device: &str) -> Option<(u32, u32)> {
    let node = std::path::Path::new(device).file_name()?.to_string_lossy().to_string();
    let text = std::fs::read_to_string(format!("{sysfs}/{node}/format")).ok()?;
    parse_format(text.trim())
}

fn parse_format(text: &str) -> Option<(u32, u32)> {
    let size = text.split_once(':')?.1.split('@').next()?;
    let (w, h) = size.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Other processes with device open, by pid (the camera watch's fallback for drivers without the usage event).
pub fn camera_holders(device: &str, own_pid: u32, proc_root: &str) -> Vec<u32> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let Ok(rdev) = std::fs::metadata(device).map(|m| m.rdev()) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(proc_root) else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        if pid == own_pid {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(st) = std::fs::metadata(fd.path()) else { continue };
            if st.rdev() == rdev && st.file_type().is_char_device() {
                found.push(pid);
                break;
            }
        }
    }
    found
}

fn fourcc(code: u32) -> String {
    String::from_utf8_lossy(&code.to_le_bytes()).trim_end().to_string()
}

/// Whether the driver took the format asked for (width, height, pixel format, bytes per line). An app holding the device,
/// or a loopback with keep_format, leaves its own, and writing frames in ours to it would show as corrupt pictures.
fn check_format(device: &str, want: (u32, u32, u32, u32), got: &PixFormat) -> std::io::Result<()> {
    let (width, height, pixelformat, bytesperline) = want;
    let fail = |text: String| Err(std::io::Error::other(text));
    if got.width != width || got.height != height {
        let (w, h) = (got.width.to_string(), got.height.to_string());
        return fail(tr("{device} is held at {width}x{height} by an app reading it").field("device", device).field("width", w).field("height", h));
    }
    if got.pixelformat != pixelformat || got.bytesperline != bytesperline {
        return fail(tr("The virtual camera {device} is set to {format}, which Telescope can't write. Close the apps using it, or reload v4l2loopback without keep_format.")
            .field("device", device).field("format", fourcc(got.pixelformat)));
    }
    Ok(())
}

impl VirtualCamera {
    pub fn open(device: &str, width: u32, height: u32, fps: u32) -> std::io::Result<VirtualCamera> {
        let file = OpenOptions::new().read(true).write(true).open(device)?;
        let (pixelformat, bytesperline, sizeimage) = wire_format(width, height);
        // SAFETY: zeroed is a valid bit pattern for both structs, and the ioctls write within their declared sizes
        unsafe {
            let mut fmt: Format = std::mem::zeroed();
            fmt.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
            fmt.pix = PixFormat {
                width, height, pixelformat, field: V4L2_FIELD_NONE, bytesperline,
                sizeimage, colorspace: V4L2_COLORSPACE_SRGB, priv_: 0, flags: 0, ycbcr_enc: 0, quantization: 0, xfer_func: 0,
            };
            if libc::ioctl(file.as_raw_fd(), VIDIOC_S_FMT, &mut fmt) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut parm: StreamParm = std::mem::zeroed();
            parm.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
            parm.numerator = 1;
            parm.denominator = fps;
            libc::ioctl(file.as_raw_fd(), VIDIOC_S_PARM, &mut parm); // only a hint for readers; fine if refused
            check_format(device, (width, height, pixelformat, bytesperline), &fmt.pix)?;
        }
        Ok(VirtualCamera { file, device: device.to_string(), width, height })
    }

    /// Writes one frame in the camera's wire_format.
    pub fn send(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.file.write_all(frame)
    }

    /// A camera writing to file, without the driver.
    #[cfg(test)]
    pub(crate) fn on_file(file: File, width: u32, height: u32) -> VirtualCamera {
        VirtualCamera { file, device: "/dev/video-test".into(), width, height }
    }
}

const _: () = assert!(std::mem::size_of::<Format>() == 208);
const _: () = assert!(std::mem::size_of::<StreamParm>() == 204);

#[cfg(test)]
mod tests {
    use super::*;

    fn sysfs(nodes: &[(&str, &str, &str)]) -> std::path::PathBuf {
        let dir = telescope_core::testing::scratch_dir("vcam-sysfs");
        for (node, name, format) in nodes {
            std::fs::create_dir_all(dir.join(node)).unwrap();
            std::fs::write(dir.join(node).join("name"), format!("{name}\n")).unwrap();
            std::fs::write(dir.join(node).join("format"), format).unwrap();
        }
        dir
    }

    // test_vcam.py::test_extra_slots_are_found_by_their_label
    #[test]
    fn extra_slots_are_found_by_their_label() {
        let dir = sysfs(&[("video11", "Telescope", ""), ("video20", "Telescope #3", ""), ("video21", "Other", "")]);
        let root = dir.to_string_lossy();
        assert_eq!(slot_device_in(&root, 0).as_deref(), Some(PHONE_DEV));
        assert_eq!(slot_device_in(&root, 2).as_deref(), Some("/dev/video20"));
        assert_eq!(slot_device_in(&root, 1), None);
        assert!(slot_ready_in(&root, 2) && !slot_ready_in(&root, 1) && slot_ready_in(&root, 0));
        let Err(why) = crate::open_node(slot_device_in(&root, 1), 1, 4, 4, 30) else { panic!("an unset slot opened") };
        assert!(why.contains("Telescope #2"), "{why}");
        assert_eq!(slot_label(0), "Telescope");
        assert_eq!(slot_label(3), "Telescope #4");
    }

    #[test]
    fn the_main_camera_is_not_another_apps_loopback() {
        let foreign = sysfs(&[("video11", "Elgato Virtual Camera", "")]);
        assert_eq!(slot_device_in(&foreign.to_string_lossy(), 0), None);
        let moved = sysfs(&[("video11", "Other", ""), ("video30", "Telescope", "")]);
        assert_eq!(slot_device_in(&moved.to_string_lossy(), 0).as_deref(), Some("/dev/video30"));
        let old = sysfs(&[("video11", "Phone Camera", "")]);
        assert_eq!(slot_device_in(&old.to_string_lossy(), 0).as_deref(), Some(PHONE_DEV));
    }

    #[test]
    fn a_main_camera_on_another_node_still_counts_as_there() {
        let hand_made = sysfs(&[("video30", "Telescope", "")]);
        assert!(main_camera_ready_in(&hand_made.to_string_lossy(), false));
        let none = sysfs(&[("video30", "Other", "")]);
        assert!(!main_camera_ready_in(&none.to_string_lossy(), false));
        assert!(main_camera_ready_in(&none.to_string_lossy(), true));
    }

    #[test]
    fn another_apps_loopback_at_the_usual_node_is_not_the_main_camera() {
        let foreign = sysfs(&[("video11", "Elgato Virtual Camera", "")]);
        assert!(!main_camera_ready_in(&foreign.to_string_lossy(), true));
        let both = sysfs(&[("video11", "Other", ""), ("video30", "Telescope", "")]);
        assert!(main_camera_ready_in(&both.to_string_lossy(), true));
        let unreadable = sysfs(&[]);
        assert!(main_camera_ready_in(&unreadable.to_string_lossy(), true), "no name to read: the usual node counts");
    }

    #[test]
    fn a_hand_made_main_camera_is_found_when_the_usual_node_is_missing() {
        let hand_made = sysfs(&[("video30", "Telescope", "")]);
        assert_eq!(slot_device_in(&hand_made.to_string_lossy(), 0).as_deref(), Some("/dev/video30"));
        assert_eq!(phone_dev_in(&hand_made.to_string_lossy()), None);
    }

    #[test]
    fn extra_slots_are_found_under_either_name() {
        let dir = sysfs(&[("video20", "Phone Camera 3", ""), ("video21", "Telescope #4", ""), ("video22", "Phone Camera 2", ""), ("video23", "Telescope #2", "")]);
        let root = dir.to_string_lossy();
        assert_eq!(slot_device_in(&root, 2).as_deref(), Some("/dev/video20"));
        assert_eq!(slot_device_in(&root, 3).as_deref(), Some("/dev/video21"));
        assert_eq!(slot_device_in(&root, 1).as_deref(), Some("/dev/video23"), "the new name wins over the old one");
        let old_only = sysfs(&[("video20", "Phone Camera 2", "")]);
        assert_eq!(slot_device_in(&old_only.to_string_lossy(), 1).as_deref(), Some("/dev/video20"));
    }

    // test_stream.py::test_another_slot_opens_its_own_camera, the Linux half
    #[test]
    fn another_slot_opens_its_own_device() {
        let dir = sysfs(&[("video9942", "Telescope #3", "")]);
        let device = slot_device_in(&dir.to_string_lossy(), 2);
        assert_eq!(device.as_deref(), Some("/dev/video9942"));
        let Err(why) = crate::open_node(device, 2, 4, 4, 30) else { panic!("no such device here") };
        assert!(why.starts_with("/dev/video9942:"), "{why}");
    }

    // test_vcam.py::test_locked_size_reads_the_drivers_format
    #[test]
    fn locked_size_reads_the_drivers_format() {
        for (text, size) in [("YU12:1280x720@30", Some((1280, 720))), ("", None), ("garbage", None), ("YU12:axb@30", None)] {
            let dir = sysfs(&[("video11", "Telescope", text)]);
            assert_eq!(locked_size_in(&dir.to_string_lossy(), "/dev/video11"), size, "{text}");
        }
        assert_eq!(locked_size_in("/nonexistent", "/dev/video11"), None);
    }

    fn pix(width: u32, height: u32, pixelformat: u32, bytesperline: u32) -> PixFormat {
        PixFormat { width, height, pixelformat, field: 1, bytesperline, sizeimage: 0, colorspace: 0, priv_: 0, flags: 0,
                    ycbcr_enc: 0, quantization: 0, xfer_func: 0 }
    }

    #[test]
    fn a_device_kept_in_another_pixel_format_is_refused() {
        let (yuyv, want) = (u32::from_le_bytes(*b"YUYV"), (4, 2, V4L2_PIX_FMT_YUV420, 4));
        assert!(check_format("/dev/video11", want, &pix(4, 2, V4L2_PIX_FMT_YUV420, 4)).is_ok());
        let why = check_format("/dev/video11", want, &pix(4, 2, yuyv, 8)).unwrap_err().to_string();
        assert!(why.contains("/dev/video11") && why.contains("YUYV"), "{why}");
        assert!(check_format("/dev/video11", want, &pix(4, 2, V4L2_PIX_FMT_YUV420, 8)).is_err());
        let why = check_format("/dev/video11", want, &pix(8, 4, V4L2_PIX_FMT_YUV420, 8)).unwrap_err().to_string();
        assert!(why.contains("8x4"), "{why}");
    }

    // test_vcam.py::test_camera_holders_skips_this_process
    #[test]
    fn camera_holders_skips_this_process() {
        let me = std::process::id();
        assert!(!camera_holders("/dev/null", me, "/proc").contains(&me));
        assert!(camera_holders("/nonexistent/device", me, "/proc").is_empty());
    }
}
