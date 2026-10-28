//! The Linux virtual camera: v4l2loopback as an output device, fed packed I420 with plain write() calls (what
//! pyvirtualcam does for vcam.py, minus the BGR to I420 conversion the Python app needs first).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;

const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_PIX_FMT_YUV420: u32 = u32::from_le_bytes(*b"YU12");
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_COLORSPACE_SRGB: u32 = 8;
const VIDIOC_S_FMT: libc::c_ulong = 0xc0d0_5605; // _IOWR('V', 5, struct v4l2_format), 208 bytes
const VIDIOC_S_PARM: libc::c_ulong = 0xc0cc_5616; // _IOWR('V', 22, struct v4l2_streamparm), 204 bytes

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

/// The first v4l2loopback device whose name is `label` (vcam.slot_device), else None.
pub fn find_by_label(label: &str) -> Option<String> {
    let mut found: Vec<_> = std::fs::read_dir("/sys/class/video4linux").ok()?.flatten().collect();
    found.sort_by_key(|e| e.file_name());
    found.into_iter().find_map(|e| {
        let name = std::fs::read_to_string(e.path().join("name")).ok()?;
        (name.trim() == label).then(|| format!("/dev/{}", e.file_name().to_string_lossy()))
    })
}

impl VirtualCamera {
    pub fn open(device: &str, width: u32, height: u32, fps: u32) -> std::io::Result<VirtualCamera> {
        let file = OpenOptions::new().read(true).write(true).open(device)?;
        let size = width * height * 3 / 2;
        // SAFETY: zeroed is a valid bit pattern for both structs, and the ioctls write within their declared sizes
        unsafe {
            let mut fmt: Format = std::mem::zeroed();
            fmt.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
            fmt.pix = PixFormat {
                width, height, pixelformat: V4L2_PIX_FMT_YUV420, field: V4L2_FIELD_NONE, bytesperline: width,
                sizeimage: size, colorspace: V4L2_COLORSPACE_SRGB, priv_: 0, flags: 0, ycbcr_enc: 0, quantization: 0, xfer_func: 0,
            };
            if libc::ioctl(file.as_raw_fd(), VIDIOC_S_FMT, &mut fmt) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut parm: StreamParm = std::mem::zeroed();
            parm.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
            parm.numerator = 1;
            parm.denominator = fps;
            libc::ioctl(file.as_raw_fd(), VIDIOC_S_PARM, &mut parm); // only a hint for readers; fine if refused
            if fmt.pix.width != width || fmt.pix.height != height {
                return Err(std::io::Error::other(format!(
                    "{device} is held at {}x{} by an app reading it", fmt.pix.width, fmt.pix.height)));
            }
        }
        Ok(VirtualCamera { file, device: device.to_string(), width, height })
    }

    pub fn send(&mut self, i420: &[u8]) -> std::io::Result<()> {
        self.file.write_all(i420)
    }
}

const _: () = assert!(std::mem::size_of::<Format>() == 208);
const _: () = assert!(std::mem::size_of::<StreamParm>() == 204);
