use super::*;
use crate::webcam_capture::gphoto_timing::Timing;
use crate::webcam_capture::gphoto_lib::{open_with, read_controls, write_control, APERTURE, EXPOSURE_COMP, FOCUS, WHITE_BALANCE};
use crate::webcam_capture::ptp::mock::{Mock, JPEG};
use crate::webcam_controls::{AUTOFOCUS, FAR, NEAR};

fn quick() -> Pacing {
    Pacing { busy_wait: Duration::ZERO, busy_tries: 5 }
}

fn open(mock: &Mock) -> PtpCamera {
    PtpCamera::open(Box::new(mock.clone()), quick()).unwrap()
}

fn values(rig: &mut PtpCamera, name: &str) -> (Vec<String>, String, bool) {
    let s = rig.read(name).unwrap_or_else(|| panic!("no {name}"));
    (s.choices, s.value, s.readonly)
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_camera_that_isnt_there_or_wont_open_a_session_is_an_error() {
    let gone = Mock::nikon();
    gone.state().unreachable = true;
    assert!(PtpCamera::open(Box::new(gone), quick()).is_err());
    let mock = Mock::nikon();
    mock.state().operations.clear();
    assert!(PtpCamera::open(Box::new(mock), quick()).is_err());
}

#[test]
fn a_session_left_open_by_someone_else_is_taken_over() {
    let mock = Mock::nikon();
    mock.state().session = true;
    assert_eq!(open(&mock).model(), "Nikon test camera");
}

#[test]
fn nikon_live_view_starts_by_itself_and_waits_out_a_busy_camera() {
    let mock = Mock::nikon();
    mock.state().busy_frames = 2;
    let mut rig = open(&mock);
    assert_eq!(rig.maker(), Maker::Nikon);
    assert_eq!(rig.preview().unwrap(), JPEG.to_vec());
    let ops = mock.ops();
    assert!(ops.contains(&0x9201), "{ops:x?}");
    assert_eq!(ops.iter().filter(|o| **o == 0x9203).count(), 3, "two busy answers and the frame");
    assert!(mock.state().log.iter().any(|(op, p, d)| *op == 0x1016 && p == &[0xD10B] && d == &[1]), "frames go to the camera's memory");
}

#[test]
fn a_camera_that_stays_busy_gives_up() {
    let mock = Mock::nikon();
    mock.state().busy_frames = 100;
    let mut rig = open(&mock);
    assert!(rig.preview().is_err());
}

#[test]
fn live_view_the_camera_ended_on_its_own_is_started_again() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    rig.preview().unwrap();
    mock.state().live = false;
    assert_eq!(rig.preview().unwrap(), JPEG.to_vec());
    assert_eq!(mock.ops().iter().filter(|o| **o == 0x9201).count(), 2);
    // A refusal that isn't that one ends the preview with the code
    mock.state().frame_code = Some(0x2002);
    assert!(rig.preview().unwrap_err().contains("0x2002"));
}

#[test]
fn the_viewfinder_setting_follows_live_view() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    assert_eq!(rig.read("viewfinder").unwrap().value, "false");
    rig.write("viewfinder", "1").unwrap();
    assert_eq!(rig.read("viewfinder").unwrap().value, "true");
    assert!(mock.state().live);
    rig.restart_live_view();
    assert!(mock.state().live && mock.ops().iter().filter(|o| **o == 0x9202).count() == 1);
    rig.write("viewfinder", "0").unwrap();
    assert!(!mock.state().live);
}

#[test]
fn nikon_menus_are_worded_like_gphotos_and_set_by_their_words() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    assert_eq!(values(&mut rig, "f-number"), (strings(&["2.8", "4", "5.6", "8"]), "5.6".into(), false));
    assert!(rig.read("aperture").is_none(), "Nikon's aperture is the standard f-number");
    rig.write("f-number", "8").unwrap();
    assert_eq!(values(&mut rig, "f-number").1, "8");
    assert_eq!(mock.state().props[&0x5007].current, Value::Int(800));
    assert!(rig.write("f-number", "1.4").is_err());
    let (comp, now, _) = values(&mut rig, "exposurecompensation");
    assert_eq!(now, "0");
    assert!(["-0.7", "-0.3", "0", "0.3", "0.7"].iter().all(|c| comp.contains(&c.to_string())), "{comp:?}");
    rig.write("exposurecompensation", "-0.7").unwrap();
    assert_eq!(mock.state().props[&0x5010].current, Value::Int(-667));
    assert_eq!(values(&mut rig, "whitebalance"), (strings(&["Automatic", "Daylight", "Tungsten", "Cloudy"]), "Automatic".into(), false));
    rig.write("whitebalance", "Tungsten").unwrap();
    assert_eq!(mock.state().props[&0x5005].current, Value::Int(6));
}

#[test]
fn a_setting_the_camera_marks_read_only_is_read_only() {
    let mock = Mock::nikon();
    mock.state().props.get_mut(&0x5007).unwrap().writable = false;
    let mut rig = open(&mock);
    assert!(values(&mut rig, "f-number").2);
    assert!(rig.write("f-number", "8").is_err());
}

#[test]
fn nikon_autofocus_waits_for_the_lens_and_focus_steps_go_near_or_far() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    rig.write("autofocusdrive", "1").unwrap();
    assert!(mock.ops().ends_with(&[0x90C1, 0x90C8]));
    rig.write("manualfocusdrive", "-200").unwrap();
    rig.write("manualfocusdrive", "50").unwrap();
    let steps: Vec<Vec<u32>> = mock.state().log.iter().filter(|(op, _, _)| *op == 0x9204).map(|(_, p, _)| p.clone()).collect();
    assert_eq!(steps, vec![vec![1, 200], vec![2, 50]]);
    assert!(rig.write("manualfocusdrive", "far").is_err());
}

#[test]
fn the_capture_threads_controls_work_on_a_nikon_unchanged() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    let controls = read_controls(&mut rig);
    let names: Vec<&str> = controls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec![FOCUS, APERTURE, EXPOSURE_COMP, WHITE_BALANCE]);
    write_control(&mut rig, FOCUS, NEAR).unwrap();
    write_control(&mut rig, FOCUS, FAR).unwrap();
    write_control(&mut rig, FOCUS, AUTOFOCUS).unwrap();
    write_control(&mut rig, APERTURE, 3).unwrap();
    assert_eq!(mock.state().props[&0x5007].current, Value::Int(800));
    let drives: Vec<Vec<u32>> = mock.state().log.iter().filter(|(op, _, _)| *op == 0x9204).map(|(_, p, _)| p.clone()).collect();
    assert_eq!(drives, vec![vec![1, 200], vec![2, 200]]);
}

#[test]
fn canon_live_view_goes_to_the_computer_and_its_frames_come_through() {
    let mock = Mock::canon();
    let mut rig = open(&mock);
    assert_eq!(rig.maker(), Maker::Canon);
    assert_eq!(rig.preview().unwrap(), JPEG.to_vec());
    let log = mock.state().log.clone();
    let remote = log.iter().position(|(op, _, _)| *op == 0x9114).unwrap();
    assert_eq!(log[remote].1, vec![1]);
    // Output device = computer (2), then the mode on
    let sets: Vec<(u32, u32)> = log.iter().filter(|(op, _, _)| *op == 0x9110).map(|(_, _, d)| (u32::from_le_bytes(d[4..8].try_into().unwrap()), u32::from_le_bytes(d[8..12].try_into().unwrap()))).collect();
    assert_eq!(sets, vec![(0xD1B0, 2), (0xD1B3, 1)]);
}

#[test]
fn canon_menus_come_from_its_events_and_are_set_by_property_record() {
    let mock = Mock::canon();
    let mut rig = open(&mock);
    assert_eq!(values(&mut rig, "aperture"), (strings(&["4", "5.6", "8"]), "5.6".into(), false));
    assert!(rig.read("f-number").is_none());
    assert_eq!(values(&mut rig, "exposurecompensation"), (strings(&["-0.3", "0", "0.3", "0.7"]), "0".into(), false));
    assert_eq!(values(&mut rig, "whitebalance"), (strings(&["Auto", "Daylight", "Cloudy"]), "Auto".into(), false));
    rig.write("aperture", "8").unwrap();
    let set = mock.state().log.iter().rev().find(|(op, _, _)| *op == 0x9110).unwrap().2.clone();
    assert_eq!(set, [12, 0, 0, 0, 0x01, 0xD1, 0, 0, 0x30, 0, 0, 0]);
    assert_eq!(values(&mut rig, "aperture").1, "8");
    rig.write("exposurecompensation", "-0.3").unwrap();
    assert_eq!(values(&mut rig, "exposurecompensation").1, "-0.3");
    assert!(rig.write("aperture", "1.4").is_err());
}

#[test]
fn canon_focus_uses_the_lens_drive_and_its_near_and_far_steps() {
    let mock = Mock::canon();
    let mut rig = open(&mock);
    let drive = rig.read("manualfocusdrive").unwrap();
    assert_eq!(drive.choices, strings(&["Near 1", "Near 2", "Near 3", "None", "Far 1", "Far 2", "Far 3"]));
    rig.write("manualfocusdrive", "Far 2").unwrap();
    rig.write("manualfocusdrive", "Near 1").unwrap();
    rig.write("autofocusdrive", "1").unwrap();
    let log = mock.state().log.clone();
    let drives: Vec<Vec<u32>> = log.iter().filter(|(op, _, _)| *op == 0x9155).map(|(_, p, _)| p.clone()).collect();
    assert_eq!(drives, vec![vec![0x8002], vec![0x0001]]);
    assert!(mock.ops().contains(&0x9154));
    assert!(rig.write("manualfocusdrive", "Sideways").is_err());
    let controls = read_controls(&mut rig);
    assert_eq!(controls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec![FOCUS, APERTURE, EXPOSURE_COMP, WHITE_BALANCE]);
}

#[test]
fn closing_turns_live_view_off_and_ends_the_session_once() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    rig.preview().unwrap();
    rig.close();
    rig.close();
    drop(rig);
    let live = mock.state().live;
    let session = mock.state().session;
    assert!(!live && !session);
    assert_eq!(mock.ops().iter().filter(|o| **o == 0x1003).count(), 1);
    let canon = Mock::canon();
    let mut rig = open(&canon);
    rig.preview().unwrap();
    drop(rig);
    assert!(!canon.state().live);
    assert!(canon.state().log.iter().any(|(op, p, _)| *op == 0x9114 && p == &[0]), "remote mode off");
}

#[test]
fn a_camera_of_a_make_we_dont_know_gives_no_live_view_and_no_settings() {
    let mock = Mock::nikon();
    mock.state().maker = Some(Maker::Other);
    let mut rig = open(&mock);
    assert_eq!(rig.maker(), Maker::Other);
    assert!(rig.preview().is_err());
    assert!(rig.write("viewfinder", "1").is_err() && rig.write("autofocusdrive", "1").is_err());
    assert!(rig.read("autofocusdrive").is_none() && rig.read("manualfocusdrive").is_none());
}

#[test]
fn the_capture_thread_streams_a_pretend_camera_end_to_end() {
    let mock = Mock::nikon();
    let shared = mock.clone();
    let mut capture = open_with("usb:001,004", Timing::default(), Box::new(move |_| Ok(Box::new(PtpCamera::open(Box::new(shared.clone()), quick())?) as Box<dyn Rig>))).unwrap();
    for _ in 0..3 {
        assert_eq!(capture.read().unwrap(), JPEG.to_vec());
    }
    assert!(mock.state().frames >= 3);
    drop(capture);
    let end = Instant::now() + Duration::from_secs(5);
    while mock.state().live && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!mock.state().live, "live view ended with the stream");
}

#[test]
fn labels_follow_how_cameras_write_their_numbers() {
    assert_eq!([2f64.powf(32.0 / 16.0), 2f64.powf(40.0 / 16.0), 2f64.powf(48.0 / 16.0)].map(aperture_label), ["4", "5.6", "8"]);
    assert_eq!(aperture_label(2.0f64.powf(0.5)), "1.4");
    assert_eq!([-8, -3, 0, 3, 4, 5, 8, 11].map(|c| stops_label(canon_stops(c))), ["-1", "-0.3", "0", "0.3", "0.5", "0.7", "1", "1.3"]);
    assert_eq!(white_balance_label(Maker::Nikon, 99), "Setting 99");
}
