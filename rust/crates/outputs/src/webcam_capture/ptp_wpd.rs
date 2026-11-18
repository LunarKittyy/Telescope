//! PTP over Windows Portable Devices: the driver that shows a camera in Explorer also takes raw PTP operations (the MTP extension
//! commands), so a DSLR needs no replacement driver. This is the `Transport` (ptp.rs) for it, and the camera list: Nikon and Canon cameras
//! among the portable devices. Windows only; everything above it (ptp_rig.rs) runs on the mock on any system.
//!
//! The device lives on a thread of its own that holds the COM apartment, and callers send it requests. A request that takes too long is
//! cancelled through an agile reference, so an unplugged camera can't hold a stream past its stop.

use super::gphoto_lib::Rig;
use super::ptp::{self, Reply, Transport};
use super::ptp_rig::{Pacing, PtpCamera};
use std::collections::HashSet;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::{AgileReference, HSTRING, PWSTR};
use windows::Win32::Devices::PortableDevices::*;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

/// What a DSLR's location starts with after the "gphoto2:" the webcam list adds.
pub const PORT_PREFIX: &str = "wpd:";
/// The part of a data transfer asked for at a time when the driver doesn't say what suits it.
const DEFAULT_CHUNK: u32 = 256 * 1024;
/// The most data one operation may carry; a live view frame is a few hundred kilobytes.
const MAX_DATA: u64 = 32 * 1024 * 1024;
/// GENERIC_READ and GENERIC_WRITE, which the driver wants before it takes commands.
const ACCESS: u32 = 0xC000_0000;
/// SECURITY_IMPERSONATION, the level Microsoft's samples open devices at.
const IMPERSONATION: u32 = 0x0002_0000;
/// How long one operation gets before it is cancelled, and how long the cancelled one gets to give up.
const DEADLINE: Duration = Duration::from_secs(10);
const AFTER_CANCEL: Duration = Duration::from_secs(3);
/// How long opening a camera waits for the worker of its last opening to let go.
const RELEASE_WAIT: Duration = Duration::from_secs(3);

/// COM on the thread that makes this, undone when it is dropped (every successful call, including the one that found COM already up, is
/// matched by one).
struct Com(bool);

impl Com {
    fn new() -> Com {
        // SAFETY: COM on this thread; undone once in drop when this call succeeded
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful CoInitializeEx in new
            unsafe { CoUninitialize() };
        }
    }
}

/// Text from a call that first says how long it is and then fills a buffer (the manager's names for a device).
fn text(mut call: impl FnMut(PWSTR, *mut u32) -> windows::core::Result<()>) -> Option<String> {
    let mut len = 0u32;
    call(PWSTR::null(), &mut len).ok()?;
    if len == 0 {
        return Some(String::new());
    }
    let mut buffer = vec![0u16; len as usize];
    call(PWSTR(buffer.as_mut_ptr()), &mut len).ok()?;
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// The cameras that can do live view over PTP, as (name, "wpd:" and the device's id): the Nikons and Canons among the portable devices.
/// Phones and other cameras are left out. Cheap, since it only asks the manager and opens no device.
pub fn detect() -> Result<Vec<(String, String)>, String> {
    let _com = Com::new();
    // SAFETY: the manager is made here and used on this thread; each id the manager allocated is freed once, right after it is copied out
    unsafe {
        let manager: IPortableDeviceManager = CoCreateInstance(&PortableDeviceManager, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        manager.RefreshDeviceList().map_err(|e| e.to_string())?;
        let mut count = 0u32;
        manager.GetDevices(std::ptr::null_mut(), &mut count).map_err(|e| e.to_string())?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut ids = vec![PWSTR::null(); count as usize];
        manager.GetDevices(ids.as_mut_ptr(), &mut count).map_err(|e| e.to_string())?;
        let mut found = Vec::new();
        for id in ids.iter().take(count as usize) {
            let device = id.to_string();
            CoTaskMemFree(Some(id.0 as *const _));
            let Ok(device) = device else { continue };
            let pnp = HSTRING::from(device.as_str());
            let maker = text(|buffer, len| manager.GetDeviceManufacturer(&pnp, buffer, len)).unwrap_or_default().to_ascii_lowercase();
            if !(maker.contains("nikon") || maker.contains("canon")) {
                continue;
            }
            let name = text(|buffer, len| manager.GetDeviceFriendlyName(&pnp, buffer, len)).filter(|n| !n.is_empty())
                .or_else(|| text(|buffer, len| manager.GetDeviceDescription(&pnp, buffer, len))).unwrap_or_else(|| "Camera".into());
            found.push((name, format!("{PORT_PREFIX}{device}")));
        }
        Ok(found)
    }
}

/// The camera on a port ("wpd:" and the device id), opened for live view.
pub fn connect(port: &str) -> Result<Box<dyn Rig>, String> {
    let id = port.strip_prefix(PORT_PREFIX).ok_or_else(|| format!("{port} isn't a portable device"))?;
    let camera = PtpCamera::open(Box::new(WpdTransport::open(id)?), Pacing::default())?;
    Ok(Box::new(camera))
}

// ── The commands ─────────────────────────────────────────────────────────

/// Where commands go: a portable device, or a stand-in in the tests.
trait Port {
    fn send(&self, params: &IPortableDeviceValues) -> windows::core::Result<IPortableDeviceValues>;
}

fn values() -> windows::core::Result<IPortableDeviceValues> {
    // SAFETY: a plain COM object with no arguments
    unsafe { CoCreateInstance(&PortableDeviceValues, None, CLSCTX_INPROC_SERVER) }
}

/// The values for one command: which command it is is a category and an id, the two halves of its key.
fn command(key: &PROPERTYKEY) -> windows::core::Result<IPortableDeviceValues> {
    let v = values()?;
    // SAFETY: the keys and the GUID are statics and locals that outlive the calls
    unsafe {
        v.SetGuidValue(&WPD_PROPERTY_COMMON_COMMAND_CATEGORY, &key.fmtid)?;
        v.SetUnsignedIntegerValue(&WPD_PROPERTY_COMMON_COMMAND_ID, key.pid)?;
    }
    Ok(v)
}

/// Sends a command and turns a failure the driver reports in its result (not only in the call) into an error.
fn send(port: &dyn Port, params: &IPortableDeviceValues) -> windows::core::Result<IPortableDeviceValues> {
    let result = port.send(params)?;
    // SAFETY: the result is live
    if let Ok(status) = unsafe { result.GetErrorValue(&WPD_PROPERTY_COMMON_HRESULT) } {
        status.ok()?;
    }
    Ok(result)
}

fn failure(why: &str) -> windows::core::Error {
    windows::core::Error::new(windows::Win32::Foundation::E_FAIL, why)
}

/// The response code a result carries, when it has one: a driver gives it with the end of a transfer, not with its start.
fn response_code(result: &IPortableDeviceValues) -> Option<u16> {
    // SAFETY: the result is live
    unsafe { result.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_RESPONSE_CODE) }.ok().map(|code| code as u16)
}

/// The response code and parameters a result carries. A result with no code is an error, since 0 would read as an answer.
fn response(result: &IPortableDeviceValues) -> windows::core::Result<(u16, Vec<u32>)> {
    let code = response_code(result).ok_or_else(|| failure("the camera's reply had no response code"))?;
    // SAFETY: the result is live; the collection and each variant are read in place
    unsafe {
        let mut params = Vec::new();
        if let Ok(collection) = result.GetIPortableDevicePropVariantCollectionValue(&WPD_PROPERTY_MTP_EXT_RESPONSE_PARAMS) {
            let mut count = 0u32;
            if collection.GetCount(&raw mut count).is_ok() {
                for i in 0..count {
                    let mut value = PROPVARIANT::default();
                    if collection.GetAt(i, &raw mut value).is_ok() {
                        params.extend(u32::try_from(&value).ok());
                    }
                }
            }
        }
        Ok((code, params))
    }
}

/// Runs one PTP operation: with the data phase it has, if any.
fn run(port: &dyn Port, op: u16, op_params: &[u32], data_out: Option<&[u8]>) -> windows::core::Result<Reply> {
    let kind = if data_out.is_some() {
        WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_WRITE
    } else if ptp::returns_data(op) {
        WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ
    } else {
        WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE
    };
    let p = command(&kind)?;
    // SAFETY: every key is a static; the collection and the values outlive the calls
    let first = unsafe {
        p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_OPERATION_CODE, op as u32)?;
        let collection: IPortableDevicePropVariantCollection = CoCreateInstance(&PortableDevicePropVariantCollection, None, CLSCTX_INPROC_SERVER)?;
        for value in op_params {
            collection.Add(&PROPVARIANT::from(*value))?;
        }
        p.SetIPortableDevicePropVariantCollectionValue(&WPD_PROPERTY_MTP_EXT_OPERATION_PARAMS, &collection)?;
        if let Some(data) = data_out {
            // The length is a 64 bit value, whatever the data's size
            p.SetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE, data.len() as u64)?;
        }
        send(port, &p)?
    };
    let first_code = response_code(&first);
    if kind.pid == WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE.pid {
        let (code, params) = response(&first)?;
        return Ok(Reply { code, params, data: Vec::new() });
    }
    // A transfer exists only for an operation the camera accepted
    // SAFETY: the string is allocated by the result and freed here once copied
    let context = unsafe {
        first.GetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT).ok().and_then(|c| {
            let text = c.to_string();
            CoTaskMemFree(Some(c.0 as *const _));
            text.ok()
        })
    };
    // A driver that opens a transfer gives its response code with the end of it, so a reply with a context and no code is an accepted one
    let Some(context) = context.filter(|_| first_code.is_none_or(|code| code == ptp::RC_OK)) else {
        let (code, params) = response(&first)?;
        return Ok(Reply { code, params, data: Vec::new() });
    };
    let context = HSTRING::from(context);
    let moved = match data_out {
        Some(data) => write_data(port, &context, data).map(|()| Vec::new()),
        None => {
            // SAFETY: the keys are statics and the result is live
            let (total, chunk) = unsafe {
                (first.GetUnsignedLargeIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE),
                 first.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_OPTIMAL_TRANSFER_BUFFER_SIZE).ok().filter(|c| *c > 0).unwrap_or(DEFAULT_CHUNK))
            };
            total.and_then(|total| read_data(port, &context, total, chunk))
        }
    };
    // The transfer is ended even when it went wrong, or the camera stays in the middle of it
    let end = command(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER)?;
    // SAFETY: the key is a static and the context outlives the call
    unsafe { end.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, &context)? };
    let ended = send(port, &end);
    let data = moved?;
    let (code, params) = response(&ended?)?;
    Ok(Reply { code, params, data })
}

/// What a transfer's total says when the driver doesn't know it yet.
fn unknown_length(total: u64) -> bool {
    total == u64::MAX || total == u64::from(u32::MAX)
}

fn read_data(port: &dyn Port, context: &HSTRING, total: u64, chunk: u32) -> windows::core::Result<Vec<u8>> {
    let unknown = unknown_length(total);
    if !unknown && total > MAX_DATA {
        return Err(failure("the camera announced more data than a frame can be"));
    }
    let mut data = Vec::with_capacity(if unknown { chunk as u64 } else { total }.min(MAX_DATA) as usize);
    loop {
        let left = if unknown { u64::from(chunk) } else { total - data.len() as u64 };
        if left == 0 {
            break;
        }
        let want = left.min(u64::from(chunk)) as u32;
        let p = command(&WPD_COMMAND_MTP_EXT_READ_DATA)?;
        // SAFETY: the keys are statics; the buffer given is the size asked for and the one returned is freed once copied
        let n = unsafe {
            p.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, context)?;
            p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_READ, want)?;
            p.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &vec![0u8; want as usize])?;
            let result = send(port, &p)?;
            let read = result.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_READ)?;
            let (mut at, mut size) = (std::ptr::null_mut::<u8>(), 0u32);
            result.GetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &mut at, &mut size)?;
            if at.is_null() {
                0
            } else {
                let n = read.min(size) as usize;
                data.extend_from_slice(std::slice::from_raw_parts(at, n));
                CoTaskMemFree(Some(at as *const _));
                n
            }
        };
        if data.len() as u64 > MAX_DATA {
            return Err(failure("the camera sent more data than a frame can be"));
        }
        // A short read ends a transfer of unknown length; for a known one it is the data ending too early
        if n < want as usize {
            if unknown {
                break;
            }
            return Err(failure("the camera ended the transfer early"));
        }
    }
    Ok(data)
}

fn write_data(port: &dyn Port, context: &HSTRING, data: &[u8]) -> windows::core::Result<()> {
    for part in data.chunks(DEFAULT_CHUNK as usize) {
        let p = command(&WPD_COMMAND_MTP_EXT_WRITE_DATA)?;
        // SAFETY: the keys are statics and the buffer outlives the call
        unsafe {
            p.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, context)?;
            p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_WRITE, part.len() as u32)?;
            p.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, part)?;
        }
        send(port, &p)?;
    }
    Ok(())
}

// ── The device and its thread ────────────────────────────────────────────

/// What the worker thread opens: the port its commands go to, and a way to cancel the one running that works from any thread.
type Made = (Box<dyn Port>, Box<dyn Fn() + Send + Sync>);

struct Request {
    op: u16,
    params: Vec<u32>,
    data: Option<Vec<u8>>,
    reply: Sender<Result<Reply, String>>,
}

/// The ids whose worker is still running, so a camera isn't opened twice at once.
fn busy() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Mutex::default)
}

/// Marks an id as in use until the worker that holds it has let the device go.
struct Claim(String);

impl Claim {
    fn take(id: &str, wait: Duration) -> Result<Claim, String> {
        let until = Instant::now() + wait;
        loop {
            if busy().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string()) {
                return Ok(Claim(id.to_string()));
            }
            if Instant::now() >= until {
                return Err("the camera is still being let go of".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        busy().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// One portable device opened for PTP operations: requests go to its own thread, which holds the apartment it lives in.
pub struct WpdTransport {
    requests: Sender<Request>,
    cancel: Box<dyn Fn() + Send + Sync>,
    deadline: Duration,
    after_cancel: Duration,
    dead: bool,
}

/// The device, closed when the worker lets go of it.
struct Device(IPortableDevice);

impl Port for Device {
    fn send(&self, params: &IPortableDeviceValues) -> windows::core::Result<IPortableDeviceValues> {
        // SAFETY: params is a live values object
        unsafe { self.0.SendCommand(0, params) }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the device is open and isn't used again
        let _ = unsafe { self.0.Close() };
    }
}

fn open_device(id: &str) -> Result<Made, String> {
    let open = || -> windows::core::Result<(IPortableDevice, AgileReference<IPortableDevice>)> {
        // SAFETY: the client info is a values object filled before the call; the device is used only on the worker's thread
        unsafe {
            let device: IPortableDevice = CoCreateInstance(&PortableDevice, None, CLSCTX_INPROC_SERVER)?;
            let info = values()?;
            info.SetStringValue(&WPD_CLIENT_NAME, &HSTRING::from("Telescope"))?;
            info.SetUnsignedIntegerValue(&WPD_CLIENT_MAJOR_VERSION, 1)?;
            info.SetUnsignedIntegerValue(&WPD_CLIENT_MINOR_VERSION, 0)?;
            info.SetUnsignedIntegerValue(&WPD_CLIENT_REVISION, 0)?;
            info.SetUnsignedIntegerValue(&WPD_CLIENT_SECURITY_QUALITY_OF_SERVICE, IMPERSONATION)?;
            info.SetUnsignedIntegerValue(&WPD_CLIENT_DESIRED_ACCESS, ACCESS)?;
            device.Open(&HSTRING::from(id), &info)?;
            let agile = AgileReference::new(&device)?;
            Ok((device, agile))
        }
    };
    let (device, agile) = open().map_err(|e| format!("the camera wouldn't open: {e}"))?;
    // Cancelling runs on a thread of its own with COM up, since the reference is resolved there
    let cancel = move || {
        let agile = agile.clone();
        let _ = std::thread::Builder::new().name("wpd-cancel".into()).spawn(move || {
            let _com = Com::new();
            if let Ok(device) = agile.resolve() {
                // SAFETY: the device is live; Cancel is meant to be called while another thread waits in SendCommand
                let _ = unsafe { device.Cancel() };
            }
        });
    };
    Ok((Box::new(Device(device)), Box::new(cancel)))
}

impl WpdTransport {
    pub fn open(id: &str) -> Result<WpdTransport, String> {
        let id = id.to_string();
        let claim = Claim::take(&id, RELEASE_WAIT)?;
        WpdTransport::with(claim, move || open_device(&id), DEADLINE, AFTER_CANCEL)
    }

    /// A transport whose worker thread builds its port with `make`, in the apartment it will use it in.
    fn with(claim: Claim, make: impl FnOnce() -> Result<Made, String> + Send + 'static, deadline: Duration, after_cancel: Duration) -> Result<WpdTransport, String> {
        let (requests, queue) = channel::<Request>();
        let (ready, started) = channel::<Result<Box<dyn Fn() + Send + Sync>, String>>();
        std::thread::Builder::new().name("wpd-camera".into()).spawn(move || {
            // Dropped last: the claim after the device and the apartment
            let _claim = claim;
            let _com = Com::new();
            let (port, cancel) = match make() {
                Ok(made) => made,
                Err(why) => {
                    let _ = ready.send(Err(why));
                    return;
                }
            };
            let _ = ready.send(Ok(cancel));
            while let Ok(request) = queue.recv() {
                let done = run(port.as_ref(), request.op, &request.params, request.data.as_deref()).map_err(|e| e.to_string());
                let _ = request.reply.send(done);
            }
        }).map_err(|e| e.to_string())?;
        let cancel = started.recv().map_err(|_| "the camera's thread ended".to_string())??;
        Ok(WpdTransport { requests, cancel, deadline, after_cancel, dead: false })
    }
}

impl Transport for WpdTransport {
    fn transact(&mut self, op: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Reply, String> {
        if self.dead {
            return Err("the camera stopped answering".into());
        }
        let (reply, answer) = channel();
        self.requests.send(Request { op, params: params.to_vec(), data: data_out.map(<[u8]>::to_vec), reply }).map_err(|_| "the camera's thread ended".to_string())?;
        match answer.recv_timeout(self.deadline) {
            Ok(done) => done,
            Err(_) => {
                // Unplugged or stuck: cancel what runs, and don't use this camera again
                (self.cancel)();
                self.dead = true;
                let _ = answer.recv_timeout(self.after_cancel);
                Err("the camera didn't answer in time".into())
            }
        }
    }
}

#[cfg(test)]
mod tests;
