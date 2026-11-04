//! What Copy diagnostics says about the computer: the graphics card, cores, memory, power and screen rate. No names, serials or ids.

use std::sync::Mutex;

/// The graphics card frame steps run on, as media tells us when it opens it.
struct Gpu {
    vendor: &'static str,
    kind: &'static str,
    name: String,
    backend: String,
    driver: String,
}

static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

/// The maker a PCI vendor id belongs to.
pub fn vendor_name(pci_id: u32) -> &'static str {
    match pci_id {
        0x10de => "NVIDIA",
        0x1002 | 0x1022 => "AMD",
        0x8086 | 0x8087 => "Intel",
        0x106b => "Apple",
        0x5143 => "Qualcomm",
        0x13b5 => "Arm",
        0x1414 => "Microsoft",
        0x15ad => "VMware",
        0x1af4 => "virtio",
        _ => "other",
    }
}

/// kind is wgpu's device type as text ("IntegratedGpu", "DiscreteGpu", "VirtualGpu", "Cpu", "Other").
fn kind_name(kind: &str) -> &'static str {
    match kind {
        "IntegratedGpu" => "integrated",
        "DiscreteGpu" => "dedicated",
        "VirtualGpu" => "virtual",
        "Cpu" => "software",
        _ => "unknown type",
    }
}

/// The graphics card in use: PCI vendor id, wgpu's device type as text, model name, backend and driver text.
pub fn set_gpu(vendor_id: u32, kind: &str, name: &str, backend: &str, driver: &str) {
    let clip = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect::<String>();
    *GPU.lock().unwrap_or_else(|e| e.into_inner()) = Some(Gpu { vendor: vendor_name(vendor_id), kind: kind_name(kind), name: clip(name), backend: clip(backend), driver: clip(driver) });
}

/// A memory size as the box would say it: the nearest size up that a computer is sold with, for the usable bytes it reports.
pub fn ram_bucket_gb(bytes: u64) -> u64 {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    [2u64, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256].into_iter().find(|s| *s as f64 >= gb * 0.98).unwrap_or(gb.round() as u64)
}

fn total_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib: u64 = text.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct MemoryStatus {
            length: u32,
            load: u32,
            total_physical: u64,
            rest: [u64; 6],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
        }
        let mut status = MemoryStatus { length: std::mem::size_of::<MemoryStatus>() as u32, load: 0, total_physical: 0, rest: [0; 6] };
        // SAFETY: the struct is MEMORYSTATUSEX with its length set
        (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.total_physical)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    None
}

/// Whether the computer runs on its battery; None for one without a battery or when it can't tell.
fn on_battery() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let (mut battery, mut mains) = (false, false);
        for entry in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
            let read = |file: &str| std::fs::read_to_string(entry.path().join(file)).unwrap_or_default().trim().to_string();
            // A mouse or a controller lists its battery here too, with the scope "Device"
            if read("scope") == "Device" {
                continue;
            }
            match read("type").as_str() {
                "Battery" => battery = true,
                "Mains" | "USB" | "USB_C" | "USB_PD" => mains |= read("online") == "1",
                _ => {}
            }
        }
        battery.then_some(!mains)
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct PowerStatus {
            ac_line: u8,
            battery_flag: u8,
            battery_percent: u8,
            system_flag: u8,
            life_time: u32,
            full_life_time: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetSystemPowerStatus(status: *mut PowerStatus) -> i32;
        }
        let mut status = PowerStatus { ac_line: 255, battery_flag: 255, battery_percent: 255, system_flag: 0, life_time: 0, full_life_time: 0 };
        // SAFETY: the struct is SYSTEM_POWER_STATUS
        let known = unsafe { GetSystemPowerStatus(&mut status) } != 0;
        // 128 in the battery flag is "no battery"; 255 in the line status is "unknown"
        (known && status.battery_flag != 128 && status.ac_line != 255).then_some(status.ac_line == 0)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    None
}

/// The main screen's refresh rate in Hz where that's cheap to ask (Windows), else None.
fn refresh_hz() -> Option<u32> {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetDC(window: *mut c_void) -> *mut c_void;
            fn ReleaseDC(window: *mut c_void, dc: *mut c_void) -> i32;
        }
        #[link(name = "gdi32")]
        unsafe extern "system" {
            fn GetDeviceCaps(dc: *mut c_void, index: i32) -> i32;
        }
        const VREFRESH: i32 = 116;
        // SAFETY: the screen's context is released again, and nothing else is done with it
        let hz = unsafe {
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let hz = GetDeviceCaps(dc, VREFRESH);
            ReleaseDC(std::ptr::null_mut(), dc);
            hz
        };
        // 0 and 1 mean the card's default
        u32::try_from(hz).ok().filter(|hz| *hz > 1)
    }
    #[cfg(not(windows))]
    None
}

/// The report's machine lines: the cores, memory, power and screen, then the graphics card.
pub fn lines() -> Vec<String> {
    let cores = std::thread::available_parallelism().map_or("?".to_string(), |n| n.get().to_string());
    let mut machine = format!("Machine: {cores} logical cores");
    if let Some(bytes) = total_memory_bytes() {
        machine.push_str(&format!(", {} GB memory", ram_bucket_gb(bytes)));
    }
    if let Some(battery) = on_battery() {
        machine.push_str(if battery { ", on battery" } else { ", on mains power" });
    }
    if let Some(hz) = refresh_hz() {
        machine.push_str(&format!(", screen {hz} Hz"));
    }
    let gpu = match GPU.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(g) => format!("Graphics: {} ({}, {}, {}), driver {}", g.name, g.vendor, g.kind, g.backend, if g.driver.is_empty() { "unknown" } else { &g.driver }),
        None => "Graphics: not opened yet".to_string(),
    };
    vec![machine, gpu]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_named_by_the_size_it_was_sold_as() {
        let gb = |x: f64| (x * (1u64 << 30) as f64) as u64;
        for (reported, sold) in [(1.9, 2), (3.8, 4), (7.7, 8), (11.7, 12), (15.4, 16), (16.0, 16), (23.4, 24), (31.2, 32), (63.0, 64), (127.0, 128), (500.0, 500)] {
            assert_eq!(ram_bucket_gb(gb(reported)), sold, "{reported} GB");
        }
    }

    #[test]
    fn the_graphics_card_line_has_the_kind_and_the_driver() {
        set_gpu(0x10de, "DiscreteGpu", "NVIDIA GeForce RTX 3060", "Vulkan", "NVIDIA 560.35.03");
        let lines = lines();
        assert_eq!(lines[1], "Graphics: NVIDIA GeForce RTX 3060 (NVIDIA, dedicated, Vulkan), driver NVIDIA 560.35.03");
        assert!(lines[0].starts_with("Machine: ") && lines[0].contains(" logical cores"), "{lines:?}");
        set_gpu(0x10005, "Cpu", "llvmpipe (LLVM 20.1.8, 256 bits)", "Gl", "");
        assert_eq!(self::lines()[1], "Graphics: llvmpipe (LLVM 20.1.8, 256 bits) (other, software, Gl), driver unknown");
    }

    #[test]
    fn makers_are_named_from_the_pci_id() {
        assert_eq!((vendor_name(0x10de), vendor_name(0x1002), vendor_name(0x8086), vendor_name(0x1234)), ("NVIDIA", "AMD", "Intel", "other"));
    }
}
