//! Which graphics card to try first, as a plain ordering the media and shell crates both feed from wgpu's adapter list (core never names wgpu).

/// The graphics API an adapter came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    Vulkan,
    Dx12,
    Metal,
    Gl,
}

/// What kind of device the adapter says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Integrated,
    Discrete,
    Other,
    Virtual,
    Cpu,
}

/// One adapter as the enumeration listed it: its API, its kind, and its place in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub api: Api,
    pub kind: Kind,
    pub index: usize,
    /// PCI vendor id, 0 when unknown.
    pub vendor: u32,
    /// Whether this is Windows, set by the caller so `order` stays the same everywhere.
    pub windows: bool,
}

/// PCI vendor id of Intel.
pub const VENDOR_INTEL: u32 = 0x8086;

/// One enumerated adapter's identity and driver text, as wgpu's `AdapterInfo` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub vendor: u32,
    pub device: u32,
    pub driver: String,
    pub driver_info: String,
}

/// Whether the text has a dotted number in it, like 31.0.101.2141.
pub(crate) fn has_version(text: &str) -> bool {
    let b = text.as_bytes();
    (1..b.len().saturating_sub(1)).any(|i| b[i] == b'.' && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit())
}

/// The driver text for the crash marker, the skip list and the log. Some drivers (Intel's Vulkan one) say no version, so when this
/// adapter's own text has none, the version comes from another listed adapter of the same vendor and device (Intel's DX12 one has it).
pub fn driver_text(own: &Info, all: &[Info]) -> String {
    let text = |i: &Info| format!("{} {}", i.driver, i.driver_info).trim().to_string();
    let mine = text(own);
    if has_version(&mine) {
        return mine;
    }
    all.iter()
        .filter(|o| o.vendor == own.vendor && o.device == own.device && has_version(&o.driver_info))
        .map(|o| format!("{} {}", own.driver, o.driver_info).trim().to_string())
        .next()
        .unwrap_or(mine)
}

/// Which kind of real card to try first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    /// Discrete cards before integrated ones.
    HighPerformance,
    /// Integrated cards before discrete ones.
    PowerSaving,
    /// Neither: the enumeration's own order decides between the two kinds.
    Unchanged,
}

/// What to prefer on this machine. On Windows that is the user's per-app graphics setting (Settings > System > Display > Graphics),
/// power saving only when it says so and high performance otherwise. Elsewhere nothing changes. Logs the answer once.
pub fn system_preference() -> Prefer {
    #[cfg(windows)]
    {
        static ANSWER: std::sync::OnceLock<Prefer> = std::sync::OnceLock::new();
        *ANSWER.get_or_init(|| {
            let (prefer, said) = match windows_gpu_preference() {
                Ok(Some(data)) => {
                    let prefer = parse_gpu_preference(&data);
                    let what = match data.trim() {
                        "GpuPreference=0;" => "Let Windows decide",
                        "GpuPreference=1;" => "power saving",
                        "GpuPreference=2;" => "high performance",
                        other => other,
                    };
                    (prefer, format!("{what}, so {}", if prefer == Prefer::PowerSaving { "power saving" } else { "high performance" }))
                }
                Ok(None) => (Prefer::HighPerformance, "none, so high performance".to_string()),
                Err(code) => (Prefer::HighPerformance, format!("couldn't read it (error {code}), so high performance")),
            };
            log::info!("Windows graphics setting for this app: {said}");
            prefer
        })
    }
    #[cfg(not(windows))]
    Prefer::Unchanged
}

/// What a `UserGpuPreferences` value like `GpuPreference=1;` asks for: 1 is power saving, anything else high performance.
pub fn parse_gpu_preference(data: &str) -> Prefer {
    let one = data.split(';').filter_map(|part| part.trim().split_once('=')).any(|(k, v)| k.trim().eq_ignore_ascii_case("GpuPreference") && v.trim() == "1");
    if one { Prefer::PowerSaving } else { Prefer::HighPerformance }
}

/// The data Windows keeps for this exe's graphics setting, None when there is none, the error code when it can't be read.
#[cfg(windows)]
fn windows_gpu_preference() -> Result<Option<String>, u32> {
    use windows::core::{w, HSTRING};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let exe = std::env::current_exe().map_err(|e| e.raw_os_error().unwrap_or(0) as u32)?;
    // value names are matched without regard to case by the registry itself
    let name = HSTRING::from(exe.as_os_str());
    let mut buf = [0u16; 512];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: buf is valid for `bytes` bytes and RegGetValueW writes at most that many
    let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\DirectX\\UserGpuPreferences"), &name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)) };
    // 2 is ERROR_FILE_NOT_FOUND: no setting for this exe, or no key at all
    match status.0 {
        0 => {}
        2 => return Ok(None),
        code => return Err(code),
    }
    let len = (bytes as usize / 2).min(buf.len());
    Ok(Some(String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0').to_string()))
}

/// Indices (`Candidate::index`) in the order to try them: real GPUs first, then Other, Virtual, Cpu. Between the real ones `prefer`
/// decides which kind goes first (Intel's Windows Vulkan driver crashes, so with a preference it goes after every other real card).
/// Inside a group Vulkan goes first on every OS (every Windows install so far ran on it) except for Intel on Windows, then DX12,
/// Metal and GL, and inside an API the driver's own order stays.
pub fn order(candidates: &[Candidate], prefer: Prefer) -> Vec<usize> {
    let tier = |k: Kind| match k {
        Kind::Integrated | Kind::Discrete => 0,
        Kind::Other => 1,
        Kind::Virtual => 2,
        Kind::Cpu => 3,
    };
    // Intel's Windows Vulkan driver crashes while opening the card, its DX12 one doesn't (gfx-rs/wgpu#9845)
    let dx12_first = |c: &Candidate| c.windows && c.vendor == VENDOR_INTEL;
    let api = |c: &Candidate| match c.api {
        Api::Vulkan if dx12_first(c) => 1,
        Api::Dx12 if dx12_first(c) => 0,
        Api::Vulkan => 0,
        Api::Dx12 => 1,
        Api::Metal => 2,
        Api::Gl => 3,
    };
    let mut sorted = candidates.to_vec();
    // gfx-rs/wgpu#9845
    let crashy = |c: &Candidate| prefer != Prefer::Unchanged && dx12_first(c) && c.api == Api::Vulkan;
    let kind_rank = |k: Kind| match (prefer, k) {
        (Prefer::HighPerformance, Kind::Integrated) | (Prefer::PowerSaving, Kind::Discrete) => 1,
        _ => 0,
    };
    sorted.sort_by_key(|c| (tier(c.kind), crashy(c), kind_rank(c.kind), api(c), c.index));
    sorted.into_iter().map(|c| c.index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(api: Api, kind: Kind, index: usize) -> Candidate {
        Candidate { api, kind, index, vendor: 0, windows: false }
    }

    fn intel(api: Api, index: usize, windows: bool) -> Candidate {
        Candidate { api, kind: Kind::Integrated, index, vendor: VENDOR_INTEL, windows }
    }

    fn info(vendor: u32, device: u32, driver: &str, driver_info: &str) -> Info {
        Info { vendor, device, driver: driver.into(), driver_info: driver_info.into() }
    }

    #[test]
    fn intel_on_windows_tries_dx12_before_vulkan() {
        let list = [intel(Api::Vulkan, 0, true), intel(Api::Dx12, 1, true), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list, Prefer::Unchanged), [1, 2, 0]);
    }

    #[test]
    fn intel_elsewhere_and_other_vendors_on_windows_keep_vulkan_first() {
        let list = [intel(Api::Dx12, 0, false), intel(Api::Vulkan, 1, false)];
        assert_eq!(order(&list, Prefer::Unchanged), [1, 0]);
        let nvidia = |api, index| Candidate { api, kind: Kind::Discrete, index, vendor: 0x10de, windows: true };
        assert_eq!(order(&[nvidia(Api::Dx12, 0), nvidia(Api::Vulkan, 1)], Prefer::Unchanged), [1, 0]);
    }

    #[test]
    fn a_driver_without_a_version_takes_it_from_the_same_card_on_another_api() {
        let vulkan = info(0x8086, 0x591b, "Intel Corporation", "Intel driver");
        let dx12 = info(0x8086, 0x591b, "Intel", "31.0.101.2141");
        let other = info(0x8086, 0x1234, "Intel", "1.2.3.4");
        assert_eq!(driver_text(&vulkan, &[vulkan.clone(), other, dx12.clone()]), "Intel Corporation 31.0.101.2141");
        assert_eq!(driver_text(&vulkan, &[vulkan.clone()]), "Intel Corporation Intel driver");
        assert_eq!(driver_text(&dx12, &[vulkan, dx12.clone()]), "Intel 31.0.101.2141");
    }

    #[test]
    fn another_vendors_version_is_not_borrowed() {
        let own = info(0x8086, 0x591b, "Intel Corporation", "Intel driver");
        let nvidia = info(0x10de, 0x591b, "NVIDIA", "546.33");
        assert_eq!(driver_text(&own, &[own.clone(), nvidia]), "Intel Corporation Intel driver");
    }

    #[test]
    fn real_gpus_come_before_software_and_odd_ones() {
        let list = [c(Api::Vulkan, Kind::Cpu, 0), c(Api::Vulkan, Kind::Other, 1), c(Api::Vulkan, Kind::Virtual, 2), c(Api::Vulkan, Kind::Integrated, 3)];
        assert_eq!(order(&list, Prefer::Unchanged), [3, 1, 2, 0]);
    }

    #[test]
    fn the_drivers_order_among_real_gpus_stays() {
        let list = [c(Api::Vulkan, Kind::Integrated, 0), c(Api::Vulkan, Kind::Discrete, 1)];
        assert_eq!(order(&list, Prefer::Unchanged), [0, 1]);
        let list = [c(Api::Vulkan, Kind::Discrete, 0), c(Api::Vulkan, Kind::Integrated, 1)];
        assert_eq!(order(&list, Prefer::Unchanged), [0, 1]);
    }

    #[test]
    fn vulkan_goes_before_dx12() {
        let list = [c(Api::Dx12, Kind::Integrated, 0), c(Api::Dx12, Kind::Discrete, 1), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list, Prefer::Unchanged), [2, 0, 1]);
    }

    #[test]
    fn a_real_vulkan_gpu_beats_a_dx12_software_adapter() {
        let list = [c(Api::Dx12, Kind::Cpu, 0), c(Api::Vulkan, Kind::Integrated, 1)];
        assert_eq!(order(&list, Prefer::Unchanged), [1, 0]);
    }

    fn laptop() -> [Candidate; 4] {
        let gtx = |api, index| Candidate { api, kind: Kind::Discrete, index, vendor: 0x10de, windows: true };
        [gtx(Api::Vulkan, 0), intel(Api::Vulkan, 1, true), intel(Api::Dx12, 2, true), gtx(Api::Dx12, 3)]
    }

    #[test]
    fn high_performance_puts_the_discrete_card_before_intel_dx12() {
        assert_eq!(order(&laptop(), Prefer::HighPerformance), [0, 3, 2, 1]);
    }

    #[test]
    fn power_saving_puts_intel_dx12_first_and_intel_vulkan_last() {
        assert_eq!(order(&laptop(), Prefer::PowerSaving), [2, 0, 3, 1]);
    }

    #[test]
    fn unchanged_keeps_the_old_order() {
        assert_eq!(order(&laptop(), Prefer::Unchanged), [0, 2, 1, 3]);
    }

    #[test]
    fn a_preference_still_keeps_software_cards_last() {
        let list = [c(Api::Vulkan, Kind::Cpu, 0), c(Api::Vulkan, Kind::Integrated, 1), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list, Prefer::HighPerformance), [2, 1, 0]);
        assert_eq!(order(&list, Prefer::PowerSaving), [1, 2, 0]);
    }

    #[test]
    fn the_windows_setting_text_is_read() {
        assert_eq!(parse_gpu_preference("GpuPreference=1;"), Prefer::PowerSaving);
        assert_eq!(parse_gpu_preference("SwapEffectUpgradeEnable=1;GpuPreference=1;"), Prefer::PowerSaving);
        assert_eq!(parse_gpu_preference("GpuPreference=2;"), Prefer::HighPerformance);
        assert_eq!(parse_gpu_preference("GpuPreference=0;"), Prefer::HighPerformance);
        assert_eq!(parse_gpu_preference(""), Prefer::HighPerformance);
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(order(&[], Prefer::Unchanged).is_empty());
    }
}
