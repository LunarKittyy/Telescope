use super::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn c920() -> Vec<Mode> {
    vec![
        Mode::new("YUYV", 640, 480, &[5.0, 10.0, 15.0, 30.0]),
        Mode::new("YUYV", 1280, 720, &[5.0, 10.0]),
        Mode::new("YUYV", 1920, 1080, &[5.0]),
        Mode::new("MJPG", 640, 480, &[15.0, 30.0]),
        Mode::new("MJPG", 1280, 720, &[15.0, 30.0, 60.0]),
        Mode::new("MJPG", 1920, 1080, &[15.0, 30.0]),
        Mode::new("MJPG", 3840, 2160, &[15.0, 30.0]),
    ]
}

fn choice(f: &str, w: u32, h: u32, fps: f64) -> Option<Choice> {
    Some(Choice { fourcc: f.into(), width: w, height: h, fps })
}

// test_webcams.py::test_raw_is_used_where_it_keeps_the_frame_rate
#[test]
fn raw_is_used_where_it_keeps_the_frame_rate() {
    assert_eq!(choose(&c920(), Some((640, 480)), 30.0), choice("YUYV", 640, 480, 30.0));
}

// test_webcams.py::test_mjpeg_is_used_where_raw_cant_keep_up
#[test]
fn mjpeg_is_used_where_raw_cant_keep_up() {
    assert_eq!(choose(&c920(), Some((1920, 1080)), 30.0), choice("MJPG", 1920, 1080, 30.0));
    assert_eq!(choose(&c920(), Some((1280, 720)), 60.0), choice("MJPG", 1280, 720, 60.0));
}

// test_webcams.py::test_auto_takes_the_largest_size_up_to_1080p_that_keeps_the_rate
#[test]
fn auto_takes_the_largest_size_up_to_1080p_that_keeps_the_rate() {
    assert_eq!(choose(&c920(), None, 30.0), choice("MJPG", 1920, 1080, 30.0));
    let raw_only = [Mode::new("YUYV", 640, 480, &[30.0]), Mode::new("YUYV", 1920, 1080, &[5.0])];
    assert_eq!(choose(&raw_only, None, 30.0), choice("YUYV", 640, 480, 30.0));
}

// test_webcams.py::test_auto_falls_back_to_the_fastest_size_when_none_keeps_the_rate
#[test]
fn auto_falls_back_to_the_fastest_size_when_none_keeps_the_rate() {
    let slow = [Mode::new("YUYV", 640, 480, &[15.0]), Mode::new("YUYV", 1280, 720, &[10.0])];
    assert_eq!(choose(&slow, None, 30.0), choice("YUYV", 640, 480, 15.0));
}

// test_webcams.py::test_a_size_the_camera_lacks_takes_the_next_bigger_one
#[test]
fn a_size_the_camera_lacks_takes_the_next_bigger_one() {
    assert_eq!(choose(&c920(), Some((1024, 576)), 30.0).unwrap().width, 1280);
    assert_eq!(choose(&c920(), Some((7680, 4320)), 30.0).unwrap().width, 3840);
}

// test_webcams.py::test_rates_close_to_the_wanted_one_count_and_the_next_faster_is_asked_for
#[test]
fn rates_close_to_the_wanted_one_count_and_the_next_faster_is_asked_for() {
    let ntsc = [Mode::new("YUYV", 1280, 720, &[29.97, 59.94])];
    assert_eq!(choose(&ntsc, Some((1280, 720)), 30.0).unwrap().fps, 29.97);
    assert_eq!(choose(&ntsc, Some((1280, 720)), 50.0).unwrap().fps, 59.94);
    assert_eq!(choose(&ntsc, Some((1280, 720)), 120.0).unwrap().fps, 59.94);
}

// test_webcams.py::test_formats_opencv_cant_read_are_ignored
#[test]
fn formats_we_cant_read_are_ignored() {
    assert_eq!(choose(&[Mode::new("H264", 1920, 1080, &[30.0])], None, 30.0), None);
}

// test_webcams.py::test_sizes_and_rates_for_the_card
#[test]
fn sizes_and_rates_for_the_card() {
    assert_eq!(sizes(&c920())[..2], [(3840, 2160), (1920, 1080)]);
    assert_eq!(rates(&c920(), Some((1280, 720))), [60, 30, 15, 10, 5]);
    assert_eq!(rates(&c920(), None)[0], 60);
}

fn c920_windows() -> Vec<Mode> {
    vec![
        Mode::new("MJPG", 1920, 1080, &[30.0]),
        Mode::new("MJPG", 640, 480, &[30.0]),
        Mode::new("YUYV", 2304, 1536, &[2.0]),
        Mode::new("YUYV", 2304, 1296, &[2.0]),
        Mode::new("YUYV", 1920, 1080, &[5.0]),
        Mode::new("YUYV", 640, 480, &[30.0]),
    ]
}

#[test]
fn sizes_that_never_reach_a_usable_rate_are_not_offered() {
    assert_eq!(sizes(&c920_windows()), [(1920, 1080), (640, 480)]);
}

#[test]
fn a_size_offers_only_the_rates_it_has() {
    assert_eq!(rates(&c920_windows(), Some((1920, 1080))), [30, 5]);
    assert_eq!(rates(&c920_windows(), Some((640, 480))), [30]);
}

#[test]
fn a_camera_with_only_slow_sizes_keeps_them_all() {
    let slow = [Mode::new("YUYV", 1280, 720, &[5.0]), Mode::new("YUYV", 640, 480, &[10.0])];
    assert_eq!(sizes(&slow), [(1280, 720), (640, 480)]);
}

#[test]
fn auto_ignores_the_slow_sizes_too() {
    let got = choose(&c920_windows(), None, 30.0).unwrap();
    assert_eq!((got.width, got.height), (1920, 1080));
}

// test_webcams.py::test_a_choice_describes_itself
#[test]
fn a_choice_describes_itself() {
    assert_eq!(choice("MJPG", 1920, 1080, 30.0).unwrap().describe(), "1920×1080, MJPEG, 30 fps");
    assert_eq!(choice("RAW", 1280, 720, 29.97).unwrap().describe(), "1280×720, 29.97 fps");
}

// test_webcams.py::test_telescopes_own_and_loopback_cameras_are_virtual
#[test]
fn telescopes_own_and_loopback_cameras_are_virtual() {
    assert!(is_virtual_on("Phone Camera", "", true) && is_virtual_on("Phone Camera 3", "", true));
    assert!(is_virtual_on("Anything", "v4l2 loopback", true));
    assert!(!is_virtual_on("HD Pro Webcam C920", "uvcvideo", true));
}

// test_webcams.py::test_virtual_cameras_are_told_apart_by_name
#[test]
fn virtual_cameras_are_told_apart_by_name() {
    for linux in [true, false] {
        let mut names = vec!["OBS Virtual Camera", "XSplit VCam"];
        if !linux {
            names.extend(["Telescope", "Telescope #2", "Unity Video Capture"]);
        }
        for n in names {
            assert!(is_virtual_on(n, "", linux), "{n}");
        }
        assert!(!is_virtual_on("Integrated Camera", "", linux) && !is_virtual_on("Cam Link 4K", "", linux));
    }
}

// test_webcams.py::test_a_card_name_with_its_cut_off_repeat_is_tidied
#[test]
fn a_card_name_with_its_cut_off_repeat_is_tidied() {
    assert_eq!(tidy_name("HP 320 FHD Webcam: HP 320 FHD W"), "HP 320 FHD Webcam");
    assert_eq!(tidy_name("Integrated Camera: Integrated C"), "Integrated Camera");
    assert_eq!(tidy_name("HD Pro Webcam C920"), "HD Pro Webcam C920");
    assert_eq!(tidy_name("Cam Link 4K: Video"), "Cam Link 4K: Video");
}

fn dev(key: &str, name: &str, loc: &str) -> Device {
    Device { key: key.into(), name: name.into(), location: loc.into(), modes: Vec::new() }
}

// test_webcams.py::test_two_cameras_of_one_model_are_numbered
#[test]
fn two_cameras_of_one_model_are_numbered() {
    let out = tell_apart(vec![dev("a", "C270", "/dev/video0"), dev("b", "C270", "/dev/video2"), dev("c", "Brio", "/dev/video4")]);
    assert_eq!(out.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["C270", "C270 (2)", "Brio"]);
}

// test_webcams.py::test_v4l2_structs_match_the_kernel
#[test]
fn v4l2_structs_match_the_kernel() {
    assert_eq!((CAP_SIZE, FMTDESC_SIZE, FRMSIZE_SIZE, FRMIVAL_SIZE), (104, 64, 44, 52));
    assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
    assert_eq!(VIDIOC_ENUM_FMT, 0xC040_5602);
    assert_eq!(VIDIOC_ENUM_FRAMESIZES, 0xC02C_564A);
    assert_eq!(VIDIOC_ENUM_FRAMEINTERVALS, 0xC034_564B);
}

struct FakeDriver;

impl Ioctl for FakeDriver {
    fn call(&self, request: u32, b: &mut [u8]) -> bool {
        let formats = ["MJPG", "H264", "YUYV"];
        match request {
            VIDIOC_ENUM_FMT => {
                let i = u32_at(b, 0) as usize;
                if i >= formats.len() {
                    return false;
                }
                put_u32(b, 44, code(formats[i]));
                true
            }
            VIDIOC_ENUM_FRAMESIZES => {
                let (i, c) = (u32_at(b, 0), fourcc(u32_at(b, 4)));
                let vals: Vec<u32> = match (c.as_str(), i) {
                    ("MJPG", 0) => vec![1, 1280, 720, 0, 0, 0, 0],
                    ("MJPG", 1) => vec![1, 1920, 1080, 0, 0, 0, 0],
                    ("YUYV", 0) => vec![3, 320, 1920, 8, 240, 1080, 8],
                    _ => return false,
                };
                for (k, v) in vals.iter().enumerate() {
                    put_u32(b, 8 + k * 4, *v);
                }
                true
            }
            VIDIOC_ENUM_FRAMEINTERVALS => {
                let (i, c, w, h) = (u32_at(b, 0), fourcc(u32_at(b, 4)), u32_at(b, 8), u32_at(b, 12));
                let vals: Vec<u32> = match (c.as_str(), w, h, i) {
                    ("MJPG", 1280, 720, 0) => vec![1, 1, 30],
                    ("MJPG", 1280, 720, 1) => vec![1, 1, 60],
                    ("MJPG", 1920, 1080, 0) => vec![1, 1001, 30000],
                    ("MJPG", _, _, _) => return false,
                    (_, _, _, 0) => vec![3, 1, 30, 1, 1, 1, 30],
                    _ => return false,
                };
                for (k, v) in vals.iter().enumerate() {
                    put_u32(b, 16 + k * 4, *v);
                }
                true
            }
            _ => false,
        }
    }
}

// test_webcams.py::test_modes_are_read_from_the_driver
#[test]
fn modes_are_read_from_the_driver() {
    let modes = list_modes(&FakeDriver);
    assert!(modes.contains(&Mode::new("MJPG", 1280, 720, &[30.0, 60.0])));
    assert!(modes.contains(&Mode::new("MJPG", 1920, 1080, &[29.97])));
    let yuyv: Vec<(u32, u32)> = modes.iter().filter(|m| m.fourcc == "YUYV").map(|m| (m.width, m.height)).collect();
    assert_eq!(yuyv, [(640, 480), (1280, 720), (1920, 1080)]);
    assert_eq!(modes.iter().find(|m| m.fourcc == "YUYV").unwrap().fps, [5.0, 10.0, 15.0, 20.0, 24.0, 25.0, 30.0]);
    assert!(modes.iter().all(|m| m.fourcc != "H264"));
}

fn node(sysfs: &Path, name: &str, label: &str, loopback: bool) -> PathBuf {
    let folder = sysfs.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("name"), format!("{label}\n")).unwrap();
    std::fs::write(folder.join("dev"), "81:0\n").unwrap();
    if loopback {
        std::fs::write(folder.join("max_openers"), "10\n").unwrap();
    }
    folder
}

type Answer = (Option<(String, String, bool)>, Vec<Mode>);

fn webcam() -> Answer {
    (Some(("uvcvideo".into(), "HD Webcam".into(), true)), vec![Mode::new("MJPG", 1280, 720, &[30.0])])
}

fn answer(driver: &str, card: &str, captures: bool, modes: Vec<Mode>) -> Answer {
    (Some((driver.into(), card.into(), captures)), modes)
}

struct Rig {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    opened: Arc<Mutex<Vec<String>>>,
    keys: Arc<Mutex<HashMap<String, String>>>,
}

fn scanner(sysfs: &Path, allow: bool) -> (LinuxScanner, Rig) {
    let rig = Rig { answers: Arc::default(), opened: Arc::default(), keys: Arc::default() };
    let (a, o, k) = (rig.answers.clone(), rig.opened.clone(), rig.keys.clone());
    let open: OpenNode = Box::new(move |path: &str| {
        let name = Path::new(path).file_name().unwrap().to_string_lossy().to_string();
        o.lock().unwrap().push(name.clone());
        a.lock().unwrap().get(&name).cloned()
    });
    let s = LinuxScanner::new(&sysfs.to_string_lossy(), "/dev", Some(allow), open, Box::new(move || k.lock().unwrap().clone()));
    (s, rig)
}

fn sysfs() -> PathBuf {
    let dir = telescope_core::testing::scratch_dir("webcams").join("video4linux");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// test_webcams.py::test_scanning_lists_capture_nodes_and_skips_the_rest
#[cfg(unix)]
#[test]
fn scanning_lists_capture_nodes_and_skips_the_rest() {
    let root = sysfs();
    node(&root, "video0", "HD Webcam", false);
    node(&root, "video1", "HD Webcam", false);
    node(&root, "video2", "Encoder", false);
    node(&root, "video10", "OBS Virtual Camera", true);
    node(&root, "video11", "Phone Camera", true);
    let (mut s, rig) = scanner(&root, false);
    rig.answers.lock().unwrap().extend([("video0".into(), webcam()), ("video1".into(), answer("uvcvideo", "HD Webcam", false, vec![])),
                                         ("video2".into(), answer("encoder", "Encoder", true, vec![]))]);
    rig.keys.lock().unwrap().insert("/dev/video0".into(), "Vendor_HD_Webcam_123".into());
    let found = s.scan();
    assert_eq!(found, [dev("Vendor_HD_Webcam_123", "HD Webcam", "/dev/video0")]);
    assert_eq!(found[0].modes, webcam().1);
    let opened = rig.opened.lock().unwrap();
    assert!(!opened.contains(&"video10".to_string()) && !opened.contains(&"video11".to_string()));
}

// test_webcams.py::test_a_node_is_only_opened_again_once_it_was_replaced
#[test]
fn a_node_is_only_opened_again_once_it_was_replaced() {
    let root = sysfs();
    let folder = node(&root, "video0", "HD Webcam", false);
    let (mut s, rig) = scanner(&root, false);
    rig.answers.lock().unwrap().insert("video0".into(), webcam());
    assert_eq!(s.scan().len(), 1);
    assert_eq!(s.scan().len(), 1);
    assert_eq!(*rig.opened.lock().unwrap(), ["video0"]);
    std::fs::remove_dir_all(&folder).unwrap();
    assert!(s.scan().is_empty());
    node(&root, "video0", "Brio", false);
    rig.answers.lock().unwrap().insert("video0".into(), answer("uvcvideo", "Brio", true, webcam().1));
    assert_eq!(s.scan().iter().map(|d| d.name.clone()).collect::<Vec<_>>(), ["Brio"]);
}

#[cfg(unix)]
fn usb(tmp: &Path, sysfs: &Path, name: &str, port: &str, serial: &str) {
    let usb = tmp.join("devices").join("pci0000:00").join(port);
    let iface = usb.join(format!("{port}:1.0"));
    std::fs::create_dir_all(&iface).unwrap();
    std::fs::write(usb.join("idVendor"), "046d\n").unwrap();
    std::fs::write(usb.join("idProduct"), "0892\n").unwrap();
    if !serial.is_empty() {
        std::fs::write(usb.join("serial"), format!("{serial}\n")).unwrap();
    }
    std::fs::write(iface.join("bInterfaceNumber"), "00\n").unwrap();
    std::os::unix::fs::symlink(&iface, node(sysfs, name, "HD Pro Webcam C920", false).join("device")).unwrap();
}

// test_webcams.py::test_usb_webcams_are_keyed_from_sysfs_without_waiting_for_udev
#[cfg(unix)]
#[test]
fn usb_webcams_are_keyed_from_sysfs_without_waiting_for_udev() {
    let root = sysfs();
    usb(root.parent().unwrap(), &root, "video0", "8-4", "ABC123");
    let (mut s, rig) = scanner(&root, false);
    rig.answers.lock().unwrap().insert("video0".into(), webcam());
    assert_eq!(s.scan()[0].key, "usb-046d:0892-ABC123-if00");
}

// test_webcams.py::test_identical_webcams_without_a_serial_are_told_apart_by_their_port
#[cfg(unix)]
#[test]
fn identical_webcams_without_a_serial_are_told_apart_by_their_port() {
    let root = sysfs();
    usb(root.parent().unwrap(), &root, "video0", "8-4", "");
    usb(root.parent().unwrap(), &root, "video2", "8-5", "");
    let (mut s, rig) = scanner(&root, false);
    s.devices = root.parent().unwrap().join("devices");
    rig.answers.lock().unwrap().extend([("video0".into(), webcam()), ("video2".into(), webcam())]);
    let keys: Vec<String> = s.scan().into_iter().map(|d| d.key).collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    assert!(keys.iter().all(|k| k.starts_with("usb-port-") && k.ends_with(":1.0")), "{keys:?}");
}

// test_webcams.py::test_a_key_that_fell_back_to_the_node_is_looked_up_again
#[cfg(unix)]
#[test]
fn a_key_that_fell_back_to_the_node_is_looked_up_again() {
    let root = sysfs();
    node(&root, "video2", "HD Webcam", false);
    let (mut s, rig) = scanner(&root, false);
    rig.answers.lock().unwrap().insert("video2".into(), webcam());
    assert_eq!(s.scan()[0].key, "video2");
    rig.keys.lock().unwrap().insert("/dev/video2".into(), "Vendor_HD_Webcam".into());
    assert_eq!(s.scan()[0].key, "Vendor_HD_Webcam");
    assert_eq!(rig.opened.lock().unwrap().len(), 1);
}

// test_webcams.py::test_a_node_that_cant_be_opened_is_tried_again
#[test]
fn a_node_that_cant_be_opened_is_tried_again() {
    let root = sysfs();
    node(&root, "video0", "HD Webcam", false);
    let (mut s, rig) = scanner(&root, false);
    assert!(s.scan().is_empty());
    rig.answers.lock().unwrap().insert("video0".into(), webcam());
    assert_eq!(s.scan().len(), 1);
}

// test_webcams.py::test_other_loopback_cameras_can_be_let_in_for_testing_but_never_virtual_ones
#[cfg(unix)]
#[test]
fn other_loopback_cameras_can_be_let_in_for_testing_but_never_virtual_ones() {
    let root = sysfs();
    node(&root, "video10", "Fake Webcam", true);
    node(&root, "video11", "Phone Camera", true);
    node(&root, "video12", "OBS Virtual Camera", true);
    let (mut s, rig) = scanner(&root, true);
    rig.answers.lock().unwrap().extend([
        ("video10".into(), answer("v4l2 loopback", "Fake Webcam", true, webcam().1)),
        ("video12".into(), answer("v4l2 loopback", "OBS Virtual Camera", true, webcam().1)),
        ("video11".into(), answer("v4l2 loopback", "Phone Camera", true, webcam().1)),
    ]);
    assert_eq!(s.scan().iter().map(|d| d.name.clone()).collect::<Vec<_>>(), ["Fake Webcam"]);
    assert_eq!(*rig.opened.lock().unwrap(), ["video10"]);
    s.scan();
    assert_eq!(rig.opened.lock().unwrap().iter().filter(|n| *n == "video10").count(), 2);
}

// test_webcams.py::test_who_uses_finds_the_program_holding_the_device
#[cfg(unix)]
#[test]
fn who_uses_finds_the_program_holding_the_device() {
    let dir = telescope_core::testing::scratch_dir("proc");
    let device = dir.join("video0");
    std::fs::write(&device, "").unwrap();
    std::fs::create_dir_all(dir.join("proc/4242/fd")).unwrap();
    std::fs::write(dir.join("proc/4242/comm"), "zoom\n").unwrap();
    std::os::unix::fs::symlink(&device, dir.join("proc/4242/fd/7")).unwrap();
    std::fs::create_dir_all(dir.join("proc/self")).unwrap();
    assert_eq!(who_uses(&device.to_string_lossy(), &dir.join("proc").to_string_lossy()), "zoom");
    assert_eq!(who_uses("/nonexistent/video9", &dir.join("proc").to_string_lossy()), "");
}

// ── DSLRs through gphoto2 ────────────────────────────────────────────────────

const DETECTED: &str = "Model                          Port                                             \n----------------------------------------------------------\nNikon DSC D5100 (PTP mode)     usb:001,004             \nMass Storage Camera            usb:001,007             \nCanon EOS 600D                 usb:002,003             \n";

#[test]
fn auto_detect_lists_cameras_that_can_do_live_view() {
    assert_eq!(parse_auto_detect(DETECTED), [("Nikon DSC D5100 (PTP mode)".to_string(), "usb:001,004".to_string()), ("Canon EOS 600D".to_string(), "usb:002,003".to_string())]);
    assert!(parse_auto_detect("").is_empty());
    assert!(parse_auto_detect("Model Port\n------\n").is_empty());
    assert!(parse_auto_detect("some error text with no table\n").is_empty());
}

#[test]
fn a_dslr_is_a_webcam_with_one_mjpeg_mode() {
    let found: Vec<Detected> = parse_auto_detect(DETECTED).into_iter().map(|(model, port)| Detected { model, port, identity: String::new() }).collect();
    let devices = gphoto_devices(&found);
    let nikon = devices.iter().find(|d| d.name.starts_with("Nikon")).unwrap();
    assert_eq!(nikon.name, "Nikon DSC D5100 (PTP mode)");
    assert_eq!(nikon.key, "gphoto2-Nikon DSC D5100 (PTP mode)");
    assert_eq!(nikon.location, "gphoto2:usb:001,004");
    assert!(is_gphoto(&nikon.location) && !is_gphoto("/dev/video0"));
    assert!(nikon.modes.len() == 1 && nikon.modes[0].compressed());
}

/// A fake /sys/bus/usb/devices with one folder per port (name, bus, device number, serial).
fn usb_tree(name: &str, cams: &[(&str, u32, u32, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("telescope-sysusb-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (folder, bus, dev, serial) in cams {
        let d = root.join(folder);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("busnum"), format!("{bus}\n")).unwrap();
        std::fs::write(d.join("devnum"), format!("{dev}\n")).unwrap();
        std::fs::write(d.join("idVendor"), "04b0\n").unwrap();
        std::fs::write(d.join("idProduct"), "0429\n").unwrap();
        if !serial.is_empty() {
            std::fs::write(d.join("serial"), format!("{serial}\n")).unwrap();
        }
    }
    root
}

fn seen(root: &Path, ports: &[&str]) -> Vec<Device> {
    let found: Vec<Detected> = ports.iter().map(|p| Detected { model: "Nikon D5100".into(), port: p.to_string(), identity: usb_identity(p, root).unwrap_or_default() }).collect();
    gphoto_devices(&found)
}

#[test]
fn a_dslr_keeps_its_key_when_a_second_one_of_its_model_comes_and_goes() {
    let root = usb_tree("twins", &[("1-2", 1, 4, ""), ("1-3", 1, 9, "")]);
    let one = seen(&root, &["usb:001,004"]);
    let two = seen(&root, &["usb:001,004", "usb:001,009"]);
    let back = seen(&root, &["usb:001,004"]);
    let first = |d: &[Device]| d.iter().find(|d| d.location == "gphoto2:usb:001,004").unwrap().key.clone();
    assert_eq!(first(&one), first(&two));
    assert_eq!(first(&one), first(&back));
    assert_ne!(two[0].key, two[1].key);
    assert_eq!(first(&one), "gphoto2-usb-port-1-2");
    // The order gphoto2 lists them in doesn't move the "(2)"
    let flipped = seen(&root, &["usb:001,009", "usb:001,004"]);
    let name_of = |d: &[Device], loc: &str| d.iter().find(|d| d.location == loc).unwrap().name.clone();
    assert_eq!(name_of(&two, "gphoto2:usb:001,009"), name_of(&flipped, "gphoto2:usb:001,009"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_replugged_dslr_keeps_its_key_though_its_device_number_changes() {
    // Same port, new device number: its settings are still its own
    let before = usb_tree("replug-a", &[("1-2", 1, 4, "")]);
    let after = usb_tree("replug-b", &[("1-2", 1, 11, "")]);
    let (a, b) = (seen(&before, &["usb:001,004"]), seen(&after, &["usb:001,011"]));
    assert_eq!(a[0].key, b[0].key);
    assert_ne!(a[0].location, b[0].location);
    // With a serial number it follows the camera to another port too
    let one = usb_tree("serial-a", &[("1-2", 1, 4, "ABC123")]);
    let moved = usb_tree("serial-b", &[("2-1.4", 2, 7, "ABC123")]);
    assert_eq!(seen(&one, &["usb:001,004"])[0].key, seen(&moved, &["usb:002,007"])[0].key);
    assert_eq!(seen(&one, &["usb:001,004"])[0].key, "gphoto2-usb-04b0:0429-ABC123");
    for r in [before, after, one, moved] {
        let _ = std::fs::remove_dir_all(r);
    }
}

#[test]
fn a_dslr_sysfs_doesnt_know_falls_back_to_its_model() {
    let root = usb_tree("none", &[]);
    assert_eq!(usb_identity("usb:001,004", &root), None);
    assert_eq!(usb_identity("serial:/dev/ttyS0", &root), None);
    let devices = seen(&root, &["usb:001,004"]);
    assert_eq!(devices[0].key, "gphoto2-Nikon D5100");
    let twins = seen(&root, &["usb:001,004", "usb:001,009"]);
    assert_ne!(twins[0].key, twins[1].key);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_dslr_is_present_while_its_usb_node_is() {
    let root = std::env::temp_dir().join(format!("telescope-usb-{}", std::process::id()));
    std::fs::create_dir_all(root.join("001")).unwrap();
    std::fs::write(root.join("001/004"), "").unwrap();
    let root_str = root.to_string_lossy().to_string();
    assert!(gphoto_present("gphoto2:usb:001,004", &root_str));
    assert!(!gphoto_present("gphoto2:usb:001,005", &root_str));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(target_os = "linux")]
mod gphoto_scanning {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn program(name: &str, script: &str) -> String {
        let dir = std::env::temp_dir().join(format!("telescope-detect-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gphoto2");
        std::fs::write(&path, format!("#!/bin/bash\n{script}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().to_string()
    }

    fn scan_until(scanner: &mut GphotoScanner, done: impl Fn(&[Device]) -> bool) -> Vec<Device> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let found = scanner.scan();
            if done(&found) || Instant::now() > until {
                return found;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_scan_never_waits_for_gphoto2_and_finds_the_camera_after() {
        let p = program("ok", &format!("sleep 0.3\ncat <<'EOF'\n{DETECTED}EOF\n"));
        let mut scanner = GphotoScanner::new(&p, Duration::from_millis(50), Duration::from_secs(5));
        let started = Instant::now();
        assert!(scanner.scan().is_empty());
        assert!(started.elapsed() < Duration::from_millis(200), "the first scan waited for gphoto2");
        let found = scan_until(&mut scanner, |d| !d.is_empty());
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_stuck_gphoto2_is_cut_off() {
        let p = program("stuck", "exec sleep 30\n");
        let started = Instant::now();
        assert_eq!(run_auto_detect(&p, Duration::from_millis(200)), Err(false));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn without_gphoto2_nothing_shows() {
        assert_eq!(run_auto_detect("/nonexistent/gphoto2", Duration::from_secs(1)), Err(true));
        let mut scanner = GphotoScanner::new("/nonexistent/gphoto2", Duration::from_millis(10), Duration::from_secs(1));
        for _ in 0..3 {
            assert!(scanner.scan().is_empty());
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
