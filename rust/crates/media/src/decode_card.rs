//! Which graphics card hardware decode opens on Windows: the one the frame steps picked, found among DXGI's adapters, instead of
//! the default (DXGI adapter 0), so a hybrid laptop set to Power saving decodes on the integrated card too.
#![cfg_attr(not(all(windows, ffmpeg)), allow(dead_code))]

use std::sync::OnceLock;

/// The vendor and device id of the card the frame steps opened, set once when they pick it.
static PICKED: OnceLock<(u32, u32)> = OnceLock::new();

pub(crate) fn set_picked(vendor: u32, device: u32) {
    let _ = PICKED.set((vendor, device));
}

/// Whether the card the frame steps picked is the one chosen in Settings: only then does a decoder that fails on it stay off the default card,
/// since a chosen card that wasn't found left the pick to the usual order and the default is as good as any.
pub(crate) fn is_chosen() -> bool {
    picked().is_some_and(|(vendor, device)| telescope_core::gpu_pick::card_id(vendor, device) == telescope_core::gpu_pick::choice())
}

pub(crate) fn picked() -> Option<(u32, u32)> {
    PICKED.get().copied()
}

/// The DXGI adapter index of the first real (not software) adapter with this vendor and device id.
/// Adapters are (index, vendor id, device id, is software).
pub(crate) fn find(adapters: &[(u32, u32, u32, bool)], vendor: u32, device: u32) -> Option<u32> {
    adapters.iter().find(|&&(_, v, d, software)| !software && v == vendor && d == device).map(|&(index, ..)| index)
}

/// FFmpeg's device string for D3D11VA (the adapter index) and the line to log about it. None keeps FFmpeg's default adapter.
#[cfg(all(windows, ffmpeg))]
pub(crate) fn d3d11va_device() -> (Option<std::ffi::CString>, String) {
    let default = |why: &str| (None, format!("Hardware decode uses Windows' default graphics card ({why})"));
    // Never opens the card here: the decode probe runs on the window's thread, and opening a card can take seconds
    let Some((vendor, device)) = picked() else { return default("no card picked for frame steps yet") };
    let listed = match adapters() {
        Ok(listed) => listed,
        Err(e) => return default(&format!("couldn't list the cards: {e}")),
    };
    let plain: Vec<(u32, u32, u32, bool)> = listed.iter().map(|a| (a.0, a.1, a.2, a.3)).collect();
    let Some(index) = find(&plain, vendor, device) else { return default("the picked card isn't among DXGI's") };
    let name = &listed[index as usize].4;
    match std::ffi::CString::new(index.to_string()) {
        Ok(text) => (Some(text), format!("Hardware decode runs on {name} (DXGI adapter {index})")),
        Err(_) => default("no adapter number"),
    }
}

/// DXGI's adapters in EnumAdapters1 order: index, vendor id, device id, software flag and name.
#[cfg(all(windows, ffmpeg))]
fn adapters() -> Result<Vec<(u32, u32, u32, bool, String)>, String> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
    // SAFETY: plain DXGI calls; the factory and adapters are COM objects released when dropped
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| e.to_string())?;
        let mut listed = Vec::new();
        // EnumAdapters1 fails with DXGI_ERROR_NOT_FOUND after the last adapter
        while let Ok(adapter) = factory.EnumAdapters1(listed.len() as u32) {
            let desc = adapter.GetDesc1().map_err(|e| e.to_string())?;
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..end]);
            let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            listed.push((listed.len() as u32, desc.VendorId, desc.DeviceId, software, name));
        }
        Ok(listed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTEL: u32 = 0x8086;
    const NVIDIA: u32 = 0x10de;

    #[test]
    fn finds_the_adapter_by_vendor_and_device() {
        let list = [(0, NVIDIA, 0x1c20, false), (1, INTEL, 0x591b, false)];
        assert_eq!(find(&list, INTEL, 0x591b), Some(1));
        assert_eq!(find(&list, NVIDIA, 0x1c20), Some(0));
    }

    #[test]
    fn needs_both_ids_to_match() {
        let list = [(0, NVIDIA, 0x1c20, false), (1, INTEL, 0x591b, false)];
        assert_eq!(find(&list, INTEL, 0x1c20), None);
        assert_eq!(find(&list, INTEL, 0x9999), None);
        assert_eq!(find(&[], INTEL, 0x591b), None);
    }

    #[test]
    fn skips_software_adapters() {
        let list = [(0, 0x1414, 0x8c, true), (1, 0x1414, 0x8c, false)];
        assert_eq!(find(&list, 0x1414, 0x8c), Some(1));
        assert_eq!(find(&list[..1], 0x1414, 0x8c), None);
    }

    #[test]
    fn takes_the_first_of_two_identical_cards() {
        let list = [(0, NVIDIA, 0x1c20, false), (1, NVIDIA, 0x1c20, false)];
        assert_eq!(find(&list, NVIDIA, 0x1c20), Some(0));
    }
}
