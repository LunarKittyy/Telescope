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
    assert_eq!(set, [12, 0, 0, 0, 0x01, 0xD1, 0, 0, 0x38, 0, 0, 0]);
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
    assert_eq!([0x28i64, 0x30, 0x38, 0x10, 0x08, 0x18].map(|raw| Words::Aperture(Maker::Canon).label(raw)), ["4", "5.6", "8", "1.4", "1", "2"]);
    assert_eq!(aperture_label(2.0f64.powf(0.5)), "1.4");
    assert_eq!([-8, -3, 0, 3, 4, 5, 8, 11].map(|c| stops_label(canon_stops(c))), ["-1", "-0.3", "0", "0.3", "0.5", "0.7", "1", "1.3"]);
    assert_eq!(white_balance_label(Maker::Nikon, 99), "Setting 99");
}

#[test]
fn canon_not_ready_waits_like_busy_and_a_list_with_missing_slots_is_ignored() {
    let mock = Mock::canon();
    mock.state().not_ready_frames = 2;
    let mut rig = open(&mock);
    assert_eq!(rig.preview().unwrap(), JPEG.to_vec());
    assert_eq!(mock.ops().iter().filter(|o| **o == 0x9153).count(), 3);
    // A list that counts more values than it holds is not applied
    let mut cam = open(&Mock::canon());
    let before = cam.canon[&0xD101].available.clone();
    let mut record = 0xD101u32.to_le_bytes().to_vec();
    record.extend_from_slice(&3u32.to_le_bytes());
    record.extend_from_slice(&9u32.to_le_bytes());
    record.extend_from_slice(&0x28u32.to_le_bytes());
    let mut event = (8 + record.len() as u32).to_le_bytes().to_vec();
    event.extend_from_slice(&0xC18Au32.to_le_bytes());
    event.extend(record);
    event.extend_from_slice(&8u32.to_le_bytes());
    event.extend_from_slice(&0u32.to_le_bytes());
    cam.t = Box::new(Fixed(event));
    cam.read_canon_events();
    assert_eq!(cam.canon[&0xD101].available, before);
}

/// A line that answers every operation with the same data.
struct Fixed(Vec<u8>);

impl Transport for Fixed {
    fn transact(&mut self, _op: u16, _params: &[u32], _data_out: Option<&[u8]>) -> Result<ptp::Reply, String> {
        Ok(ptp::Reply { code: ptp::RC_OK, params: Vec::new(), data: self.0.clone() })
    }
}

#[test]
fn a_property_range_that_would_overflow_ends_instead_of_panicking() {
    let mock = Mock::nikon();
    mock.state().props.insert(0x5010, PropDesc { code: 0x5010, kind: 8, writable: true, current: Value::Int(0), form: Form::Range { min: i64::MAX - 1, max: i64::MAX, step: 1 } });
    let mut rig = open(&mock);
    assert!(rig.read("exposurecompensation").is_some());
}

fn waits(rig: &mut PtpCamera) {
    rig.photo_wait = Duration::from_millis(50);
}

#[test]
fn nikon_takes_a_photo_into_its_memory_and_the_file_comes_with_its_name() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    let shots = rig.capture(1, &|| false).unwrap();
    assert_eq!(shots, vec![Shot { name: "DSC_0001.JPG".into(), data: JPEG.to_vec() }]);
    let log = mock.state().log.clone();
    let at = |op: u16| log.iter().position(|(o, _, _)| *o == op).unwrap_or_else(|| panic!("{op:#x} never sent"));
    assert_eq!(log[at(0x90C0)].1, vec![0xFFFF_FFFF]);
    assert!(log.iter().any(|(op, p, d)| *op == 0x1016 && p == &[0xD10B] && d == &[1]), "photos go to the camera's memory");
    assert!(at(0x90C0) < at(0x90C8) && at(0x90C8) < at(0x101B), "{:x?}", mock.ops());
}

#[test]
fn nikon_waits_for_both_files_of_a_pair_and_reports_the_one_that_came_when_the_other_didnt() {
    let mock = Mock::nikon();
    mock.state().photo_files.push(("DSC_0001.NEF".into(), vec![1, 2, 3]));
    let mut rig = open(&mock);
    let shots = rig.capture(2, &|| false).unwrap();
    assert_eq!(shots.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["DSC_0001.JPG", "DSC_0001.NEF"]);
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    waits(&mut rig);
    let Err(Incomplete { why, shots, .. }) = rig.capture(2, &|| false) else { panic!("only one file was made") };
    assert_eq!(shots.len(), 1);
    assert!(why.starts_with("1 of 2 files came"), "{why}");
}

#[test]
fn nikon_that_never_says_the_photo_is_there_times_out_with_nothing() {
    let mock = Mock::nikon();
    mock.state().silent_photos = true;
    let mut rig = open(&mock);
    waits(&mut rig);
    let Err(Incomplete { why, shots, .. }) = rig.capture(1, &|| false) else { panic!("nothing was reported") };
    assert!(shots.is_empty() && why.contains("in time"), "{why}");
}

#[test]
fn a_photo_taken_while_the_picture_went_hidden_is_never_fetched() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    let Err(Incomplete { why, shots, .. }) = rig.capture(1, &|| true) else { panic!("the picture was hidden") };
    assert!(shots.is_empty() && why.contains("hidden"), "{why}");
    assert!(!mock.ops().contains(&0x1009), "{:x?}", mock.ops());
}

#[test]
fn a_camera_without_the_capture_operation_says_it_cant_be_triggered() {
    let mock = Mock::nikon();
    mock.state().operations.retain(|op| *op != 0x90C0);
    let mut rig = open(&mock);
    let Err(Incomplete { why, .. }) = rig.capture(1, &|| false) else { panic!("the camera can't capture") };
    assert!(why.contains("can't be triggered"), "{why}");
}

#[test]
fn canon_presses_half_then_full_sends_the_photo_to_the_computer_and_lets_the_camera_go() {
    let mock = Mock::canon();
    let mut rig = open(&mock);
    let shots = rig.capture(1, &|| false).unwrap();
    assert_eq!(shots, vec![Shot { name: "IMG_0001.JPG".into(), data: JPEG.to_vec() }]);
    let log = mock.state().log.clone();
    let sent: Vec<(u16, Vec<u32>)> = log.iter().filter(|(op, _, _)| matches!(op, 0x9128 | 0x9129 | 0x9117)).map(|(op, p, _)| (*op, p.clone())).collect();
    assert_eq!(sent, vec![(0x9128, vec![1, 0]), (0x9128, vec![2, 0]), (0x9129, vec![2]), (0x9129, vec![1]), (0x9117, vec![0x1000])]);
    assert!(log.iter().any(|(op, _, d)| *op == 0x9110 && d.get(4..8) == Some(&0xD11Cu32.to_le_bytes()[..]) && d.get(8..12) == Some(&4u32.to_le_bytes()[..])), "to the computer");
}

#[test]
fn canon_that_makes_a_pair_gives_both_and_one_that_stays_silent_still_gets_its_buttons_released() {
    let mock = Mock::canon();
    mock.state().photo_files.push(("IMG_0001.CR3".into(), vec![9; 4]));
    let mut rig = open(&mock);
    assert_eq!(rig.capture(2, &|| false).unwrap().len(), 2);
    let silent = Mock::canon();
    silent.state().silent_photos = true;
    let mut rig = open(&silent);
    waits(&mut rig);
    assert!(rig.capture(1, &|| false).is_err());
    assert_eq!(silent.ops().iter().filter(|o| **o == 0x9129).count(), 2, "both buttons come up");
}

#[test]
fn a_photo_of_an_unknown_make_is_refused() {
    let mock = Mock::nikon();
    mock.state().maker = Some(Maker::Other);
    let mut rig = open(&mock);
    assert!(rig.capture(1, &|| false).is_err());
}

#[test]
fn the_stills_flow_takes_a_photo_through_the_rig() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    let shots = crate::webcam_capture::gphoto_stills::take_photo(&mut rig, &crate::webcam_capture::gphoto_stills::Wish::from_options(&serde_json::json!({}))).unwrap();
    assert_eq!(shots.len(), 1);
}

#[test]
fn a_raw_file_over_the_transports_limit_comes_in_pieces_and_whole() {
    let mock = Mock::nikon();
    let big: Vec<u8> = (0..(2 << 20) + 12345).map(|i| (i % 251) as u8).collect();
    {
        let mut s = mock.state();
        s.photo_files = vec![("DSC_0002.NEF".into(), big.clone())];
        // A whole-file read of this would fail, as it does over WPD
        s.max_data = Some(1 << 20);
    }
    let mut rig = open(&mock);
    let shots = rig.capture(1, &|| false).unwrap();
    assert_eq!(shots[0].data, big);
    let pieces: Vec<Vec<u32>> = mock.state().log.iter().filter(|(op, _, _)| *op == 0x101B).map(|(_, p, _)| p.clone()).collect();
    assert_eq!(pieces.len(), 3);
    assert!(pieces.iter().all(|p| p[2] <= 1 << 20));
    assert!(!mock.ops().contains(&0x1009), "the whole-file read wasn't used");
}

#[test]
fn nikon_lets_go_of_what_it_held_in_memory_and_stops_asking_when_it_doesnt_know_the_handle() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    rig.capture(1, &|| false).unwrap();
    assert!(mock.state().log.iter().any(|(op, p, _)| *op == 0x100B && p == &[0xFFFF_0001, 0]));
    let mock = Mock::nikon();
    mock.state().delete_code = Some(0x2009);
    mock.state().photo_files.push(("DSC_0001.NEF".into(), vec![1, 2, 3]));
    let mut rig = open(&mock);
    rig.capture(2, &|| false).unwrap();
    assert_eq!(mock.ops().iter().filter(|o| **o == 0x100B).count(), 1, "asked once for the photo, not for each file");
    // The next photo asks again
    rig.capture(2, &|| false).unwrap();
    assert_eq!(mock.ops().iter().filter(|o| **o == 0x100B).count(), 2);
}

#[test]
fn a_file_the_nikon_put_on_its_card_is_fetched_and_never_deleted() {
    let mock = Mock::nikon();
    mock.state().to_card = true;
    mock.state().folder_first = true;
    let mut rig = open(&mock);
    let shots = rig.capture(1, &|| false).unwrap();
    assert_eq!(shots[0].name, "DSC_0001.JPG");
    assert!(!mock.ops().contains(&0x100B) && !mock.ops().contains(&0x90C3), "nothing on the card or its folders is deleted: {:x?}", mock.ops());
    assert!(mock.state().objects.contains_key(&0xFFFF_0001) && mock.state().objects.contains_key(&0x10));
}

#[test]
fn a_folder_the_nikon_announces_is_not_taken_for_the_photo() {
    let mock = Mock::nikon();
    mock.state().folder_first = true;
    let mut rig = open(&mock);
    let shots = rig.capture(1, &|| false).unwrap();
    assert_eq!(shots[0].name, "DSC_0001.JPG");
}

#[test]
fn a_nikon_that_is_busy_through_a_long_exposure_still_gives_the_photo() {
    let mock = Mock::nikon();
    // Far more busy answers than the tries a single command gets
    mock.state().ready_busy = 500;
    let mut rig = open(&mock);
    assert_eq!(rig.capture(1, &|| false).unwrap().len(), 1);
}

#[test]
fn a_stop_ends_the_wait_for_a_photo_and_keeps_live_view_from_starting_again() {
    let mock = Mock::nikon();
    mock.state().silent_photos = true;
    let mut rig = open(&mock);
    let stop = Arc::new(AtomicBool::new(false));
    rig.set_stop(stop.clone());
    stop.store(true, Ordering::SeqCst);
    let start = Instant::now();
    let Err(Incomplete { why, .. }) = rig.capture(1, &|| false) else { panic!("the camera was stopped") };
    assert!(why.contains("stopped") && start.elapsed() < Duration::from_secs(5), "{why}");
    assert!(rig.write("viewfinder", "1").is_err());
}

#[test]
fn a_camera_that_stays_busy_ending_live_view_is_not_shot() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    rig.preview().unwrap();
    mock.state().end_busy = true;
    let Err(Incomplete { why, .. }) = rig.capture(1, &|| false) else { panic!("live view was still on") };
    assert!(why.contains("busy"), "{why}");
    assert!(!mock.ops().contains(&0x90C0), "no photo was started");
}

#[test]
fn nikon_quality_is_a_setting_with_the_files_it_makes_in_its_names() {
    let mock = Mock::nikon();
    let mut rig = open(&mock);
    let (choices, current, _) = values(&mut rig, "imagequality");
    assert_eq!(choices, strings(&["JPEG Normal", "JPEG Fine", "NEF (Raw)", "NEF+Fine"]));
    assert_eq!(current, "JPEG Normal");
    rig.write("imagequality", "NEF+Fine").unwrap();
    assert!(mock.state().log.iter().any(|(op, p, d)| *op == 0x1016 && p == &[0x5004] && d == &[7]));
}

#[test]
fn canon_files_on_the_card_are_fetched_without_telling_the_camera_a_transfer_is_done() {
    let mock = Mock::canon();
    mock.state().canon_props.remove(&0xD11C);
    let mut rig = open(&mock);
    assert_eq!(rig.capture(1, &|| false).unwrap().len(), 1);
    assert!(!mock.ops().contains(&0x9117), "{:x?}", mock.ops());
}

#[test]
fn an_eos_m_takes_one_press_and_a_canon_that_wont_let_go_is_reset() {
    let mock = Mock::canon();
    mock.state().model = Some("Canon EOS M50".into());
    let mut rig = open(&mock);
    rig.capture(1, &|| false).unwrap();
    let sent: Vec<(u16, Vec<u32>)> = mock.state().log.iter().filter(|(op, _, _)| matches!(op, 0x9128 | 0x9129)).map(|(op, p, _)| (*op, p.clone())).collect();
    assert_eq!(sent, vec![(0x9128, vec![3, 0]), (0x9129, vec![3])]);
    let stuck = Mock::canon();
    stuck.state().release_code = Some(0x2002);
    let mut rig = open(&stuck);
    let Err(Incomplete { why, .. }) = rig.capture(1, &|| false) else { panic!("the release failed") };
    assert!(why.contains("let go"), "{why}");
    let modes: Vec<Vec<u32>> = stuck.state().log.iter().filter(|(op, _, _)| *op == 0x9114).map(|(_, p, _)| p.clone()).collect();
    assert_eq!(modes, vec![vec![1], vec![0], vec![1]], "remote mode was left and entered again");
}

#[test]
fn eos_m_is_told_from_the_marks_by_its_name() {
    assert!(capture::is_eos_m("Canon EOS M50") && capture::is_eos_m("Canon EOS M"));
    assert!(!capture::is_eos_m("Canon EOS 5D Mark IV") && !capture::is_eos_m("Canon EOS R5"));
}
