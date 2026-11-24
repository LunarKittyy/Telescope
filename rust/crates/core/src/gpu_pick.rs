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
    /// PCI device id, 0 when unknown.
    pub device: u32,
}

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
/// decides which kind goes first. Inside a group Vulkan goes first, then DX12, Metal and GL, and inside an API the driver's own order
/// stays, so a card whose Vulkan driver fails to open still has its DX12 adapter behind it.
pub fn order(candidates: &[Candidate], prefer: Prefer) -> Vec<usize> {
    let tier = |k: Kind| match k {
        Kind::Integrated | Kind::Discrete => 0,
        Kind::Other => 1,
        Kind::Virtual => 2,
        Kind::Cpu => 3,
    };
    let api = |c: &Candidate| match c.api {
        Api::Vulkan => 0,
        Api::Dx12 => 1,
        Api::Metal => 2,
        Api::Gl => 3,
    };
    let mut sorted = candidates.to_vec();
    let kind_rank = |k: Kind| match (prefer, k) {
        (Prefer::HighPerformance, Kind::Integrated) | (Prefer::PowerSaving, Kind::Discrete) => 1,
        _ => 0,
    };
    sorted.sort_by_key(|c| (tier(c.kind), kind_rank(c.kind), api(c), c.index));
    sorted.into_iter().map(|c| c.index).collect()
}

/// The graphics card Settings asked for as `card_id` text, "" for Automatic. Set once at start, before the window opens a card.
static CHOICE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// The real cards the last enumeration found, in the order they were tried.
static LISTED: std::sync::Mutex<Vec<Listed>> = std::sync::Mutex::new(Vec::new());
/// The card the window opened, for Settings to show.
static OPENED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A real card as Settings lists it: the id it is saved by and the name it is shown by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub name: String,
}

/// A card's id, the same on every API (Vulkan and DX12 can word its name differently): its PCI vendor and device.
pub fn card_id(vendor: u32, device: u32) -> String {
    format!("{vendor:04x}:{device:04x}")
}

pub fn set_choice(id: &str) {
    *CHOICE.lock().unwrap_or_else(|e| e.into_inner()) = id.trim().to_ascii_lowercase();
}

/// The card chosen in Settings, empty for Automatic.
pub fn choice() -> String {
    CHOICE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Notes the card the window opened.
pub fn set_opened(name: &str) {
    *OPENED.lock().unwrap_or_else(|e| e.into_inner()) = name.trim().to_string();
}

/// The card the window opened (empty before one did, or when the window draws on the processor).
pub fn opened() -> String {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Remembers which real cards (integrated or discrete) exist, once each, for the Settings list. `names` is indexed like `candidates`.
pub fn note_listed(candidates: &[Candidate], names: &[String]) {
    let mut found: Vec<Listed> = Vec::new();
    for i in order(candidates, Prefer::Unchanged) {
        let c = &candidates[i];
        let real = matches!(c.kind, Kind::Integrated | Kind::Discrete);
        let id = card_id(c.vendor, c.device);
        if let (true, Some(name)) = (real, names.get(c.index)) {
            if !found.iter().any(|f| f.id == id) {
                found.push(Listed { id, name: name.trim().to_string() });
            }
        }
    }
    *LISTED.lock().unwrap_or_else(|e| e.into_inner()) = found;
}

/// The real cards found so far (empty until a card was looked for).
pub fn listed() -> Vec<Listed> {
    LISTED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The adapters (`Candidate::index`) that are the chosen card, on every API.
pub fn chosen(candidates: &[Candidate], choice: &str) -> Vec<usize> {
    if choice.trim().is_empty() {
        return Vec::new();
    }
    candidates.iter().filter(|c| card_id(c.vendor, c.device).eq_ignore_ascii_case(choice.trim())).map(|c| c.index).collect()
}

/// `ordered` (candidate indices, best first) with the chosen card's adapters moved to the front, in their own order. The rest follow, so
/// a chosen card that fails or sits on the skip list still leaves something to open. No choice, or one that matches no adapter (the card
/// was taken out), changes nothing.
pub fn with_choice(ordered: Vec<usize>, candidates: &[Candidate], choice: &str) -> Vec<usize> {
    let wanted = chosen(candidates, choice);
    let (first, rest): (Vec<usize>, Vec<usize>) = ordered.into_iter().partition(|i| wanted.contains(i));
    first.into_iter().chain(rest).collect()
}

/// `ordered` with one adapter (`Candidate::index`) moved to the front, for a power preference set in the environment. Unchanged when it isn't
/// there or isn't a real card.
pub fn move_to_front(mut ordered: Vec<usize>, candidates: &[Candidate], index: usize) -> Vec<usize> {
    // A power preference only picks between real cards, never software or odd adapters (`none` can name one)
    if candidates.iter().any(|c| c.index == index && !matches!(c.kind, Kind::Integrated | Kind::Discrete)) {
        return ordered;
    }
    if let Some(at) = ordered.iter().position(|&i| i == index) {
        let found = ordered.remove(at);
        ordered.insert(0, found);
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(api: Api, kind: Kind, index: usize) -> Candidate {
        Candidate { api, kind, index, vendor: 0, device: 0 }
    }

    fn intel(api: Api, index: usize) -> Candidate {
        Candidate { api, kind: Kind::Integrated, index, vendor: 0x8086, device: 0x591b }
    }

    fn info(vendor: u32, device: u32, driver: &str, driver_info: &str) -> Info {
        Info { vendor, device, driver: driver.into(), driver_info: driver_info.into() }
    }

    #[test]
    fn intel_tries_vulkan_before_dx12_and_keeps_dx12_as_the_fallback() {
        let list = [intel(Api::Dx12, 0), intel(Api::Vulkan, 1), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list, Prefer::Unchanged), [1, 2, 0], "the Intel DX12 adapter stays in the list, behind its Vulkan one");
    }

    #[test]
    fn a_driver_without_a_version_takes_it_from_the_same_card_on_another_api() {
        let vulkan = info(0x8086, 0x591b, "Intel Corporation", "Intel driver");
        let dx12 = info(0x8086, 0x591b, "Intel", "31.0.101.2141");
        let other = info(0x8086, 0x1234, "Intel", "1.2.3.4");
        assert_eq!(driver_text(&vulkan, &[vulkan.clone(), other, dx12.clone()]), "Intel Corporation 31.0.101.2141");
        assert_eq!(driver_text(&vulkan, std::slice::from_ref(&vulkan)), "Intel Corporation Intel driver");
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
        let gtx = |api, index| Candidate { api, kind: Kind::Discrete, index, vendor: 0x10de, device: 0x1c20 };
        [gtx(Api::Vulkan, 0), intel(Api::Vulkan, 1), intel(Api::Dx12, 2), gtx(Api::Dx12, 3)]
    }

    #[test]
    fn high_performance_puts_the_discrete_card_before_intel() {
        assert_eq!(order(&laptop(), Prefer::HighPerformance), [0, 3, 1, 2]);
    }

    #[test]
    fn power_saving_puts_intel_before_the_discrete_card() {
        assert_eq!(order(&laptop(), Prefer::PowerSaving), [1, 2, 0, 3]);
    }

    #[test]
    fn unchanged_keeps_the_old_order() {
        assert_eq!(order(&laptop(), Prefer::Unchanged), [0, 1, 2, 3]);
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
    fn a_chosen_card_goes_first_and_keeps_its_own_order() {
        let ordered = order(&laptop(), Prefer::HighPerformance);
        assert_eq!(ordered, [0, 3, 1, 2]);
        assert_eq!(with_choice(ordered.clone(), &laptop(), "8086:591b"), [1, 2, 0, 3], "both Intel adapters first, Vulkan before DX12");
        assert_eq!(with_choice(ordered.clone(), &laptop(), "10DE:1C20"), ordered);
    }

    #[test]
    fn automatic_and_a_missing_card_change_nothing() {
        assert_eq!(with_choice(vec![3, 1], &laptop(), ""), [3, 1]);
        assert_eq!(with_choice(vec![3, 1], &laptop(), "  "), [3, 1]);
        assert_eq!(with_choice(vec![3, 1], &laptop(), "1234:5678"), [3, 1]);
        assert!(chosen(&laptop(), "1234:5678").is_empty());
    }

    #[test]
    fn a_chosen_card_matches_its_adapters_on_every_api() {
        assert_eq!(chosen(&laptop(), "8086:591b"), [1, 2]);
    }

    #[test]
    fn the_list_has_each_real_card_once_by_id() {
        let names: Vec<String> = ["GTX", "Intel", "Intel (R)", "GTX", "llvmpipe"].iter().map(|s| s.to_string()).collect();
        let mut list = laptop().to_vec();
        list.push(c(Api::Vulkan, Kind::Cpu, 4));
        note_listed(&list, &names);
        assert_eq!(listed(), [Listed { id: "10de:1c20".into(), name: "GTX".into() }, Listed { id: "8086:591b".into(), name: "Intel".into() }]);
    }

    #[test]
    fn one_adapter_can_be_moved_first() {
        let list = laptop();
        assert_eq!(move_to_front(vec![0, 1, 2, 3], &list, 3), [3, 0, 1, 2]);
        assert_eq!(move_to_front(vec![0, 1, 2, 3], &list, 9), [0, 1, 2, 3]);
        assert_eq!(move_to_front(vec![0, 1, 2, 3], &list, 1), [1, 0, 2, 3]);
        let soft = [c(Api::Vulkan, Kind::Discrete, 0), c(Api::Vulkan, Kind::Cpu, 1)];
        assert_eq!(move_to_front(vec![0, 1], &soft, 1), [0, 1], "software never goes first");
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(order(&[], Prefer::Unchanged).is_empty());
    }
}
