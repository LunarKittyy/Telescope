//! PTP over Windows Portable Devices: the driver that shows a camera in Explorer also takes raw PTP operations (the MTP extension
//! commands), so a DSLR needs no replacement driver. This is the `Transport` (ptp.rs) for it, and the camera list: Nikon and Canon cameras
//! among the portable devices. Windows only; everything above it (ptp_rig.rs) runs on the mock on any system.

use super::gphoto_lib::Rig;
use super::ptp::{self, Reply, Transport};
use super::ptp_rig::{Pacing, PtpCamera};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Devices::PortableDevices::*;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

/// What a DSLR's location starts with after the "gphoto2:" the webcam list adds.
pub const PORT_PREFIX: &str = "wpd:";
/// The part of a data transfer asked for at a time when the driver doesn't say what suits it.
const DEFAULT_CHUNK: u32 = 256 * 1024;
/// GENERIC_READ and GENERIC_WRITE, which the driver wants before it takes commands.
const ACCESS: u32 = 0xC000_0000;
/// SECURITY_IMPERSONATION, the level Microsoft's samples open devices at.
const IMPERSONATION: u32 = 0x0002_0000;

fn start() {
    // SAFETY: COM on this thread; a thread that already has it (in any mode) keeps it
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
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
    start();
    // SAFETY: the manager is made here and used on this thread; the id array it allocates is freed after each id is copied out
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
            let Ok(device) = id.to_string() else { continue };
            CoTaskMemFree(Some(id.0 as *const _));
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

/// One portable device opened for PTP operations.
pub struct WpdTransport {
    device: IPortableDevice,
}

// SAFETY: the device is made in the free-threaded COM apartment, whose objects may be used from any thread, and is only ever used by one at a time
unsafe impl Send for WpdTransport {}

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

impl WpdTransport {
    pub fn open(id: &str) -> Result<WpdTransport, String> {
        start();
        let open = || -> windows::core::Result<IPortableDevice> {
            // SAFETY: the client info is a values object filled before the call; the device is used only through this wrapper
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
                Ok(device)
            }
        };
        open().map(|device| WpdTransport { device }).map_err(|e| format!("the camera wouldn't open: {e}"))
    }

    /// Sends one command and gives its answer.
    fn send(&self, params: &IPortableDeviceValues) -> windows::core::Result<IPortableDeviceValues> {
        // SAFETY: params is a live values object
        unsafe { self.device.SendCommand(0, params) }
    }

    /// The response code and parameters a result carries.
    fn response(result: &IPortableDeviceValues) -> (u16, Vec<u32>) {
        // SAFETY: the result is live; the collection and each variant are read in place
        unsafe {
            let code = result.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_RESPONSE_CODE).unwrap_or(0) as u16;
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
            (code, params)
        }
    }

    unsafe fn run(&self, op: u16, op_params: &[u32], data_out: Option<&[u8]>) -> windows::core::Result<Reply> {
        let kind = if data_out.is_some() {
            &WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_WRITE
        } else if ptp::returns_data(op) {
            &WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITH_DATA_TO_READ
        } else {
            &WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE
        };
        let p = command(kind)?;
        p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_OPERATION_CODE, op as u32)?;
        let collection: IPortableDevicePropVariantCollection = CoCreateInstance(&PortableDevicePropVariantCollection, None, CLSCTX_INPROC_SERVER)?;
        for value in op_params {
            collection.Add(&PROPVARIANT::from(*value))?;
        }
        p.SetIPortableDevicePropVariantCollectionValue(&WPD_PROPERTY_MTP_EXT_OPERATION_PARAMS, &collection)?;
        if let Some(data) = data_out {
            p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE, data.len() as u32)?;
        }
        let first = self.send(&p)?;
        let (code, params) = Self::response(&first);
        if kind == &WPD_COMMAND_MTP_EXT_EXECUTE_COMMAND_WITHOUT_DATA_PHASE {
            return Ok(Reply { code, params, data: Vec::new() });
        }
        // A transfer exists only for an operation the camera accepted
        let context = first.GetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT).ok().and_then(|c| {
            let text = c.to_string().ok();
            CoTaskMemFree(Some(c.0 as *const _));
            text
        });
        let Some(context) = context.filter(|_| code == ptp::RC_OK || code == 0) else { return Ok(Reply { code, params, data: Vec::new() }) };
        let context = HSTRING::from(context);
        let moved = match data_out {
            Some(data) => self.write_data(&context, data).map(|()| Vec::new()),
            None => {
                let total = first.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_TOTAL_DATA_SIZE).unwrap_or(0);
                let chunk = first.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_OPTIMAL_TRANSFER_BUFFER_SIZE).ok().filter(|c| *c > 0).unwrap_or(DEFAULT_CHUNK);
                self.read_data(&context, total, chunk)
            }
        };
        // The transfer is ended even when it went wrong, or the camera stays in the middle of it
        let end = command(&WPD_COMMAND_MTP_EXT_END_DATA_TRANSFER)?;
        end.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, &context)?;
        let (code, params) = self.send(&end).map(|r| Self::response(&r)).unwrap_or((code, params));
        Ok(Reply { code, params, data: moved? })
    }

    unsafe fn read_data(&self, context: &HSTRING, total: u32, chunk: u32) -> windows::core::Result<Vec<u8>> {
        let mut data = Vec::with_capacity(total as usize);
        while (data.len() as u32) < total {
            let want = chunk.min(total - data.len() as u32);
            let p = command(&WPD_COMMAND_MTP_EXT_READ_DATA)?;
            p.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, context)?;
            p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_READ, want)?;
            p.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &vec![0u8; want as usize])?;
            let result = self.send(&p)?;
            let read = result.GetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_READ)?;
            let (mut at, mut size) = (std::ptr::null_mut::<u8>(), 0u32);
            result.GetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, &mut at, &mut size)?;
            if at.is_null() {
                break;
            }
            let n = read.min(size) as usize;
            data.extend_from_slice(std::slice::from_raw_parts(at, n));
            CoTaskMemFree(Some(at as *const _));
            if n == 0 {
                break;
            }
        }
        Ok(data)
    }

    unsafe fn write_data(&self, context: &HSTRING, data: &[u8]) -> windows::core::Result<()> {
        for part in data.chunks(DEFAULT_CHUNK as usize) {
            let p = command(&WPD_COMMAND_MTP_EXT_WRITE_DATA)?;
            p.SetStringValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_CONTEXT, context)?;
            p.SetUnsignedIntegerValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_NUM_BYTES_TO_WRITE, part.len() as u32)?;
            p.SetBufferValue(&WPD_PROPERTY_MTP_EXT_TRANSFER_DATA, part)?;
            self.send(&p)?;
        }
        Ok(())
    }
}

impl Transport for WpdTransport {
    fn transact(&mut self, op: u16, params: &[u32], data_out: Option<&[u8]>) -> Result<Reply, String> {
        // SAFETY: every COM object used inside is made and dropped within the call
        unsafe { self.run(op, params, data_out) }.map_err(|e| e.to_string())
    }
}

impl Drop for WpdTransport {
    fn drop(&mut self) {
        // SAFETY: the device is open and isn't used again
        unsafe {
            let _ = self.device.Close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
