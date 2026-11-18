//! Which graphics card hardware decode opens: the one the frame steps picked. On Windows it is found among DXGI's adapters instead of the
//! default (DXGI adapter 0), so a hybrid laptop set to Power saving decodes on the integrated card too. On Linux it is the VA-API render node
//! of that card instead of the first one.
#![cfg_attr(not(all(any(windows, target_os = "linux"), ffmpeg)), allow(dead_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The vendor id, device id and PCI bus address (empty when unknown) of the card the frame steps opened. It is replaced when they open
/// another one, so an unplugged card doesn't stay the answer.
static PICKED: Mutex<Option<(u32, u32, String)>> = Mutex::new(None);

/// Records the card the frame steps opened. Returns whether it is a different card than before, in which case what was learned about
/// the old one (the node that failed, the card decoder found) is forgotten.
pub(crate) fn set_picked(vendor: u32, device: u32, bus: &str) -> bool {
    let now = (vendor, device, bus.trim().to_string());
    let mut picked = PICKED.lock().unwrap_or_else(|e| e.into_inner());
    let changed = picked.as_ref() != Some(&now);
    *picked = Some(now);
    drop(picked);
    if changed {
        DEFAULT_NODE.store(false, Ordering::SeqCst);
        NODE_USED.store(false, Ordering::SeqCst);
    }
    changed
}

/// The picked card's render node failed, so VA-API's default node is used from now on.
static DEFAULT_NODE: AtomicBool = AtomicBool::new(false);
/// A render node of the picked card was handed to VA-API.
static NODE_USED: AtomicBool = AtomicBool::new(false);

/// A VA-API decoder failed: whether it was on the picked card's render node, which is then left for the default one (true), or not.
pub(crate) fn retry_on_default_node(hw_is_vaapi: bool) -> bool {
    hw_is_vaapi && NODE_USED.load(Ordering::SeqCst) && !DEFAULT_NODE.swap(true, Ordering::SeqCst)
}

/// Whether the card the frame steps picked is the one chosen in Settings: only then does a decoder that fails on it stay off the default card,
/// since a chosen card that wasn't found left the pick to the usual order and the default is as good as any.
pub(crate) fn is_chosen() -> bool {
    picked().is_some_and(|(vendor, device)| telescope_core::gpu_pick::card_id(vendor, device) == telescope_core::gpu_pick::choice())
}

pub(crate) fn picked() -> Option<(u32, u32)> {
    PICKED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|p| (p.0, p.1))
}

#[cfg(target_os = "linux")]
fn picked_bus() -> String {
    PICKED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|p| p.2.clone()).unwrap_or_default()
}

/// A DRM render node: its device path, PCI vendor and device id, and PCI bus address (empty when unknown).
#[cfg(target_os = "linux")]
pub(crate) type RenderNode = (String, u32, u32, String);

/// The render node of the picked card. With a bus address, only the node at exactly that address counts. Without one, the node with the
/// vendor and device id counts when it is the only one, since identical cards can't be told apart. None keeps VA-API's default node.
#[cfg(target_os = "linux")]
pub(crate) fn find_node(nodes: &[RenderNode], vendor: u32, device: u32, bus: &str) -> Option<String> {
    let bus = bus.trim();
    if !bus.is_empty() {
        return nodes.iter().find(|n| n.3.eq_ignore_ascii_case(bus)).map(|n| n.0.clone());
    }
    let mut same = nodes.iter().filter(|n| n.1 == vendor && n.2 == device);
    match (same.next(), same.next()) {
        (Some(only), None) => Some(only.0.clone()),
        _ => None,
    }
}

/// Parses sysfs's `0x1002` style id.
#[cfg(target_os = "linux")]
pub(crate) fn parse_id(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// The render nodes under `class` (`/sys/class/drm`) and `dev` (`/dev/dri`), sorted by name.
#[cfg(target_os = "linux")]
pub(crate) fn render_nodes(class: &std::path::Path, dev: &std::path::Path) -> Vec<RenderNode> {
    let Ok(entries) = std::fs::read_dir(class) else { return Vec::new() };
    let mut nodes: Vec<RenderNode> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.strip_prefix("renderD")?;
            let device = entry.path().join("device");
            let read = |file: &str| std::fs::read_to_string(device.join(file)).ok().and_then(|t| parse_id(&t));
            let bus = std::fs::canonicalize(&device).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
            Some((dev.join(&name).to_string_lossy().into_owned(), read("vendor")?, read("device")?, bus))
        })
        .collect();
    nodes.sort();
    nodes
}

/// The render node VA-API should open and its path, or None for VA-API's default node (no card picked, none matches, or the picked
/// card's node failed earlier).
#[cfg(all(target_os = "linux", ffmpeg))]
pub(crate) fn vaapi_device() -> Option<(std::ffi::CString, String)> {
    if DEFAULT_NODE.load(Ordering::SeqCst) {
        return None;
    }
    let (vendor, device) = picked()?;
    let nodes = render_nodes(std::path::Path::new("/sys/class/drm"), std::path::Path::new("/dev/dri"));
    let node = find_node(&nodes, vendor, device, &picked_bus())?;
    NODE_USED.store(true, Ordering::SeqCst);
    Some((std::ffi::CString::new(node.clone()).ok()?, node))
}

/// The DXGI adapter index of the first real (not software) adapter with this vendor and device id.
/// Adapters are (index, vendor id, device id, is software).
#[cfg_attr(not(windows), allow(dead_code))]
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

    #[cfg(target_os = "linux")]
    fn node(name: &str, vendor: u32, device: u32, bus: &str) -> RenderNode {
        (format!("/dev/dri/{name}"), vendor, device, bus.into())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_node_is_found_by_exact_bus_address() {
        let nodes = [node("renderD128", NVIDIA, 0x1c20, "0000:01:00.0"), node("renderD129", NVIDIA, 0x1c20, "0000:02:00.0")];
        assert_eq!(find_node(&nodes, NVIDIA, 0x1c20, "0000:02:00.0").as_deref(), Some("/dev/dri/renderD129"));
        assert_eq!(find_node(&nodes, NVIDIA, 0x1c20, "0000:02:00.0".to_uppercase().as_str()).as_deref(), Some("/dev/dri/renderD129"));
        // A known address that no node has isn't matched by vendor and device
        assert_eq!(find_node(&nodes, NVIDIA, 0x1c20, "0000:09:00.0"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn without_a_bus_address_only_a_unique_vendor_and_device_matches() {
        let nodes = [node("renderD128", INTEL, 0x591b, "0000:00:02.0"), node("renderD129", NVIDIA, 0x1c20, "0000:01:00.0")];
        assert_eq!(find_node(&nodes, NVIDIA, 0x1c20, "").as_deref(), Some("/dev/dri/renderD129"));
        assert_eq!(find_node(&nodes, 0x1002, 0x7550, "0000:03:00.0"), None);
        assert_eq!(find_node(&[], INTEL, 0x591b, ""), None);
        let twins = [node("renderD128", NVIDIA, 0x1c20, "0000:01:00.0"), node("renderD129", NVIDIA, 0x1c20, "0000:02:00.0")];
        assert_eq!(find_node(&twins, NVIDIA, 0x1c20, ""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sysfs_ids_parse() {
        assert_eq!(parse_id("0x1002\n"), Some(0x1002));
        assert_eq!(parse_id("nonsense"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn render_nodes_are_read_from_a_sysfs_like_folder() {
        let root = telescope_core::testing::scratch_dir("render-nodes");
        let bus = root.join("pci/0000:03:00.0");
        std::fs::create_dir_all(&bus).unwrap();
        std::fs::write(bus.join("vendor"), "0x1002\n").unwrap();
        std::fs::write(bus.join("device"), "0x7550\n").unwrap();
        let class = root.join("class");
        std::fs::create_dir_all(class.join("renderD128")).unwrap();
        std::fs::create_dir_all(class.join("card1")).unwrap();
        std::os::unix::fs::symlink(&bus, class.join("renderD128/device")).unwrap();
        assert_eq!(render_nodes(&class, std::path::Path::new("/dev/dri")), vec![node("renderD128", 0x1002, 0x7550, "0000:03:00.0")]);
    }

    #[test]
    fn a_new_card_replaces_the_old_one_and_what_was_learned_about_it() {
        assert!(set_picked(INTEL, 0x591b, "0000:00:02.0"));
        assert!(!set_picked(INTEL, 0x591b, "0000:00:02.0"));
        NODE_USED.store(true, Ordering::SeqCst);
        assert!(retry_on_default_node(true));
        assert!(!retry_on_default_node(true), "the default node is only moved to once");
        assert!(set_picked(NVIDIA, 0x1c20, "0000:01:00.0"));
        assert_eq!(picked(), Some((NVIDIA, 0x1c20)));
        assert!(!retry_on_default_node(true), "no node was handed out for the new card yet");
        assert!(!retry_on_default_node(false));
    }

    #[test]
    fn takes_the_first_of_two_identical_cards() {
        let list = [(0, NVIDIA, 0x1c20, false), (1, NVIDIA, 0x1c20, false)];
        assert_eq!(find(&list, NVIDIA, 0x1c20), Some(0));
    }
}
