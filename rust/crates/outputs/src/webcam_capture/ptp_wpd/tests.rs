use super::*;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::E_FAIL;

const READ_OP: u16 = 0x9203;
const WRITE_OP: u16 = 0x1016;
const PLAIN_OP: u16 = 0x9201;

/// A portable device as the WPD documentation says it answers, so the commands can be checked without a camera: lengths are 64 bit, a
/// transfer has a context, and every command can fail through the HRESULT in its result.
struct Fake {
    data: Vec<u8>,
    announce: u64,
    fail: Option<u32>,
    response: u16,
    /// A driver that gives the response code of a transfer only with its end, as a real camera's does.
    quiet_start: bool,
    /// No response code at the end of a transfer, or on an operation without data.
    quiet_end: bool,
    seen: RefCell<Vec<u32>>,
    offset: Cell<usize>,
    wrote: RefCell<Vec<u8>>,
    wrote_total: Cell<u64>,
}

impl Fake {
    fn serving(data: Vec<u8>) -> Fake {
        Fake { announce: data.len() as u64, data, fail: None, response: ptp::RC_OK, quiet_start: false, quiet_end: false, seen: RefCell::default(), offset: Cell::new(0), wrote: RefCell::default(), wrote_total: Cell::new(0) }
    }

    fn saw(&self, key: &PROPERTYKEY) -> usize {
        self.seen.borrow().iter().filter(|id| **id == key.pid).count()
    }
}

impl Port for Fake {
    fn send(&self, p: &IPortableDeviceValues) -> windows::core::Result<IPortableDeviceValues> {
        // SAFETY: values objects made and read inside the call; the buffer a read hands back is freed after it is copied
        unsafe {
            let id = p.GetUnsignedIntegerValue(&WPD_PROPERTY_COMMON_COMMAND_ID)?;
            self.seen.borrow_mut().push(id);
            let r = values()?;
            if self.fail == Some(id) {
                r.SetErrorValue(&WPD_PROPERTY_COMMON_HRESULT, E_FAIL)?;
                return Ok(r);
            }
            let opens = id == WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ.pid || id == WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_WRITE.pid;
            let closes = id == WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER.pid || id == WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE.pid;
            if !(self.quiet_start && opens) && !(self.quiet_end && closes) {
                r.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_RESPONSE_CODE, self.response as u32)?;
            }
            let context = HSTRING::from("transfer-1");
            if id == WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ.pid {
                if self.response == ptp::RC_OK {
                    r.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, &context)?;
                    r.SetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE, self.announce)?;
                }
            } else if id == WPD_COMMAND_MTP_EXT_READ_DATA.pid {
                let want = p.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_READ)? as usize;
                let at = self.offset.get();
                let n = want.min(self.data.len() - at);
                r.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_READ, n as u32)?;
                r.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &self.data[at..at + n])?;
                self.offset.set(at + n);
            } else if id == WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_WRITE.pid {
                // The length has to be read as the 64 bit value it is
                self.wrote_total.set(p.GetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE)?);
                r.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, &context)?;
            } else if id == WPD_COMMAND_MTP_EXT_WRITE_DATA.pid {
                let (mut at, mut size) = (std::ptr::null_mut::<u8>(), 0u32);
                p.GetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &mut at, &mut size)?;
                self.wrote.borrow_mut().extend_from_slice(std::slice::from_raw_parts(at, size as usize));
                CoTaskMemFree(Some(at as *const _));
                r.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_WRITTEN, size)?;
            }
            Ok(r)
        }
    }
}

#[test]
fn a_transfer_whose_length_is_64_bit_is_read_whole() {
    let _com = Com::new();
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let port = Fake::serving(data.clone());
    let reply = run(&port, READ_OP, &[], None).unwrap();
    assert_eq!((reply.code, reply.data), (ptp::RC_OK, data));
    assert_eq!(port.saw(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER), 1);
}

#[test]
fn a_transfer_opened_without_a_response_code_goes_ahead_and_takes_its_code_from_the_end() {
    let _com = Com::new();
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let mut port = Fake::serving(data.clone());
    port.quiet_start = true;
    let reply = run(&port, READ_OP, &[], None).unwrap();
    assert_eq!((reply.code, reply.data), (ptp::RC_OK, data));
    let mut port = Fake::serving(vec![1, 2, 3]);
    port.quiet_start = true;
    let reply = run(&port, WRITE_OP, &[0x5007], Some(&[9; 5])).unwrap();
    assert_eq!(reply.code, ptp::RC_OK);
    assert_eq!(*port.wrote.borrow(), [9; 5]);
}

#[test]
fn a_reply_with_no_response_code_where_one_is_due_is_an_error_not_a_zero() {
    let _com = Com::new();
    let mut port = Fake::serving(vec![1, 2, 3]);
    port.quiet_start = true;
    port.quiet_end = true;
    let why = run(&port, READ_OP, &[], None).unwrap_err();
    assert!(why.message().contains("no response code"), "{why}");
    let mut port = Fake::serving(Vec::new());
    port.quiet_end = true;
    let why = run(&port, 0x1002, &[1], None).unwrap_err();
    assert!(why.message().contains("no response code"), "{why}");
    // A camera that does give its codes is as before
    assert_eq!(run(&Fake::serving(Vec::new()), 0x1002, &[1], None).unwrap().code, ptp::RC_OK);
}

#[test]
fn the_transfer_length_round_trips_as_64_bit_even_past_32_bits() {
    // Windows converts a 64 bit value for the 32 bit getter when it fits, but a length past 4 GiB must survive as it is
    let _com = Com::new();
    let v = values().unwrap();
    // SAFETY: a values object made here
    unsafe {
        v.SetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE, 1 << 40).unwrap();
        assert_eq!(v.GetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE).unwrap(), 1 << 40);
    }
}

#[test]
fn data_sent_to_the_camera_announces_its_length_as_64_bit_and_arrives_whole() {
    let _com = Com::new();
    let data: Vec<u8> = (0..(DEFAULT_CHUNK as usize + 5)).map(|i| (i % 251) as u8).collect();
    let port = Fake::serving(Vec::new());
    let reply = run(&port, WRITE_OP, &[0x5007], Some(&data)).unwrap();
    assert_eq!(reply.code, ptp::RC_OK);
    assert_eq!(port.wrote_total.get(), data.len() as u64);
    assert_eq!(*port.wrote.borrow(), data);
    assert_eq!(port.saw(&WPD_COMMAND_MTP_EXT_WRITE_DATA), 2, "two chunks");
}

#[test]
fn a_transfer_of_unknown_length_is_read_until_a_short_chunk_and_never_reserved_for() {
    let _com = Com::new();
    for sentinel in [u64::MAX, u64::from(u32::MAX)] {
        let mut port = Fake::serving(vec![7; 1000]);
        port.announce = sentinel;
        assert_eq!(run(&port, READ_OP, &[], None).unwrap().data, vec![7; 1000]);
    }
}

#[test]
fn a_transfer_that_ends_before_its_length_is_an_error() {
    let _com = Com::new();
    let mut port = Fake::serving(vec![1; 600]);
    port.announce = 1000;
    assert!(run(&port, READ_OP, &[], None).is_err());
    assert_eq!(port.saw(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER), 1);
}

#[test]
fn a_length_bigger_than_a_frame_is_refused_before_anything_is_read() {
    let _com = Com::new();
    let mut port = Fake::serving(vec![1; 10]);
    port.announce = 1 << 40;
    assert!(run(&port, READ_OP, &[], None).is_err());
    assert_eq!(port.saw(&WPD_COMMAND_MTP_EXT_READ_DATA), 0);
    assert_eq!(port.saw(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER), 1);
}

#[test]
fn a_failure_the_driver_reports_in_its_result_is_an_error_at_each_step() {
    let _com = Com::new();
    for failing in [WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ, WPD_COMMAND_MTP_EXT_READ_DATA, WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER] {
        let mut port = Fake::serving(vec![1; 10]);
        port.fail = Some(failing.pid);
        assert!(run(&port, READ_OP, &[], None).is_err(), "{failing:?}");
    }
    let mut port = Fake::serving(Vec::new());
    port.fail = Some(WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE.pid);
    assert!(run(&port, PLAIN_OP, &[], None).is_err());
}

#[test]
fn an_operation_the_camera_refuses_gives_its_code_and_starts_no_transfer() {
    let _com = Com::new();
    let mut port = Fake::serving(vec![1; 10]);
    port.response = ptp::RC_DEVICE_BUSY;
    let reply = run(&port, READ_OP, &[], None).unwrap();
    assert_eq!((reply.code, reply.data.len()), (ptp::RC_DEVICE_BUSY, 0));
    assert_eq!(port.saw(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER), 0);
    let mut port = Fake::serving(Vec::new());
    port.response = 0x2002;
    assert_eq!(run(&port, PLAIN_OP, &[3, 4], None).unwrap().code, 0x2002);
}

/// A port that never answers until it is cancelled.
struct Stuck(Arc<AtomicBool>);

impl Port for Stuck {
    fn send(&self, _p: &IPortableDeviceValues) -> windows::core::Result<IPortableDeviceValues> {
        while !self.0.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(failure("cancelled"))
    }
}

#[test]
fn a_camera_that_stops_answering_is_cancelled_after_the_deadline_and_not_used_again() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let make = move || -> Result<Made, String> {
        let on_cancel = flag.clone();
        Ok((Box::new(Stuck(flag)), Box::new(move || on_cancel.store(true, Ordering::SeqCst))))
    };
    let claim = Claim::take("test-stuck", Duration::from_secs(1)).unwrap();
    let mut transport = WpdTransport::with(claim, make, Duration::from_millis(150), Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    assert!(transport.transact(READ_OP, &[], None).unwrap_err().contains("in time"));
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(transport.transact(READ_OP, &[], None).unwrap_err().contains("stopped answering"));
    // Letting go of the transport lets the worker end and the camera be opened again
    drop(transport);
    assert!(Claim::take("test-stuck", Duration::from_secs(3)).is_ok());
}

#[test]
fn a_camera_is_not_opened_twice_at_once() {
    let first = Claim::take("test-twice", Duration::from_millis(50)).unwrap();
    assert!(Claim::take("test-twice", Duration::from_millis(100)).is_err());
    drop(first);
    assert!(Claim::take("test-twice", Duration::from_millis(100)).is_ok());
}

#[test]
fn listing_portable_devices_runs_and_only_names_nikons_and_canons() {
    let found = detect().expect("the manager answers");
    assert!(found.iter().all(|(_, port)| port.starts_with(PORT_PREFIX)));
}

#[test]
#[ignore = "needs a Nikon or Canon plugged in"]
fn a_real_camera_gives_live_view_frames() {
    let (name, port) = detect().unwrap().into_iter().next().expect("no camera plugged in");
    let mut rig = connect(&port).expect("opened");
    println!("camera: {name}");
    let frame = rig.preview().expect("a frame");
    assert!(frame.starts_with(&[0xFF, 0xD8]));
    if let Ok(path) = std::env::var("DSLR_TEST_JPG") {
        std::fs::write(path, &frame).unwrap();
    }
    rig.close();
}
