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

/// Whether Vulkan goes before DX12 on Windows: always, unless Settings asks for DirectX 12 first.
pub fn vulkan_first_asked() -> bool {
    !dx12_first_chosen()
}

/// Whether the saved choice (`dx12`, `vulkan`, or empty for none yet) asks for DirectX 12 first. Only `dx12` does.
pub fn dx12_first_for(saved: &str) -> bool {
    saved.trim() == "dx12"
}

static DX12_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static OPENED_API: std::sync::Mutex<Option<Api>> = std::sync::Mutex::new(None);

/// Sets what Settings said ("DirectX 12 first" is true), once at start before the window opens a card. Only Windows has the choice.
pub fn set_dx12_first(on: bool) {
    DX12_FIRST.store(on && cfg!(windows), std::sync::atomic::Ordering::Relaxed);
}

/// What was set at start, whatever WGPU_BACKEND says: what Settings compares its own choice with to know a restart is due.
pub fn dx12_first_set() -> bool {
    DX12_FIRST.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether Settings' "DirectX 12 first" counts: not when WGPU_BACKEND names the API itself (developers), which always wins.
pub fn dx12_first_chosen() -> bool {
    dx12_first_set() && std::env::var_os("WGPU_BACKEND").is_none_or(|v| v.is_empty())
}

/// Whether the card the window opened is Intel's, whose API the choice never moves (`order`).
pub fn opened_is_intel() -> bool {
    window_card().starts_with(&format!("{VENDOR_INTEL:04x}:"))
}

/// The API the window's card opened on, for the status line.
pub fn opened_api() -> Option<Api> {
    *OPENED_API.lock().unwrap_or_else(|e| e.into_inner())
}

/// Which device is being opened, since the window and the frame pipeline want different things from an Intel card on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The window's own device, which draws the preview.
    Window,
    /// The frame pipeline's device, which shares decoded pictures with the D3D11 decoder on DX12 only.
    Pipeline,
}

/// Whether DX12 goes before Vulkan for this adapter: on Windows only when Settings asks for DirectX 12 first. Intel ignores that:
/// its window stays on Vulkan (its DX12 driver leaks memory) and its pipeline on DX12 (see `order`).
fn dx12_first(c: &Candidate, role: Role, vulkan_first: bool) -> bool {
    c.windows && if c.vendor == VENDOR_INTEL { role == Role::Pipeline } else { !vulkan_first }
}

/// Whether the window's device on this adapter can also run the frame pipeline. Not on Intel's Vulkan adapter in Windows: the pipeline wants
/// the card's DX12 adapter there (see `order`) and opens its own device on it, so the preview is then painted from the pipeline's pictures
/// in memory instead of from the card.
pub fn window_serves_pipeline(api: Api, vendor: u32, windows: bool) -> bool {
    !(windows && vendor == VENDOR_INTEL && api == Api::Vulkan)
}

/// Indices (`Candidate::index`) in the order to try them: real GPUs first, then Other, Virtual, Cpu. Between the real ones `prefer`
/// decides which kind goes first. Inside a group Vulkan goes first, unless Settings asks for DirectX 12 first on Windows, where only DX12
/// shares decoded pictures with the decoder. Metal and GL follow, and inside an API the driver's own order stays. Intel in Windows
/// ignores the choice: its window goes Vulkan first (its DX12 driver leaks memory) and its pipeline DX12 first.
pub fn order(candidates: &[Candidate], prefer: Prefer, role: Role) -> Vec<usize> {
    order_by(candidates, prefer, role, vulkan_first_asked())
}

fn order_by(candidates: &[Candidate], prefer: Prefer, role: Role, vulkan_first: bool) -> Vec<usize> {
    let tier = |k: Kind| match k {
        Kind::Integrated | Kind::Discrete => 0,
        Kind::Other => 1,
        Kind::Virtual => 2,
        Kind::Cpu => 3,
    };
    let api = |c: &Candidate| match c.api {
        Api::Vulkan if dx12_first(c, role, vulkan_first) => 1,
        Api::Dx12 if dx12_first(c, role, vulkan_first) => 0,
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
/// The `card_id` of the card the window opened, so the frame pipeline can land on the same card.
static WINDOW_CARD: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

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

/// Notes the card the window opened, by name, by `card_id` and by the API it opened on.
pub fn set_opened(name: &str, id: &str, api: Api) {
    *OPENED_API.lock().unwrap_or_else(|e| e.into_inner()) = Some(api);
    *OPENED.lock().unwrap_or_else(|e| e.into_inner()) = name.trim().to_string();
    *WINDOW_CARD.lock().unwrap_or_else(|e| e.into_inner()) = id.to_ascii_lowercase();
}

/// The `card_id` of the card the window opened, empty before one did.
pub fn window_card() -> String {
    WINDOW_CARD.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The card the window opened (empty before one did, or when the window draws on the processor).
pub fn opened() -> String {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Remembers which real cards (integrated or discrete) exist, once each, for the Settings list. `names` is indexed like `candidates`.
pub fn note_listed(candidates: &[Candidate], names: &[String]) {
    let mut found: Vec<Listed> = Vec::new();
    // the role and the API choice only order one card's APIs, which doesn't show in the list, so the list keeps one order whatever is chosen
    for i in order_by(candidates, Prefer::Unchanged, Role::Pipeline, true) {
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

/// `ordered` with the whole card that adapter (`Candidate::index`) belongs to moved to the front, keeping its adapters' order, for a power
/// preference set in the environment. Unchanged when it isn't there or isn't a real card.
pub fn move_to_front(ordered: Vec<usize>, candidates: &[Candidate], index: usize) -> Vec<usize> {
    // A power preference only picks between real cards, never software or odd adapters (`none` can name one)
    let Some(picked) = candidates.iter().find(|c| c.index == index && matches!(c.kind, Kind::Integrated | Kind::Discrete)) else {
        return ordered;
    };
    with_choice(ordered, candidates, &card_id(picked.vendor, picked.device))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(api: Api, kind: Kind, index: usize) -> Candidate {
        Candidate { api, kind, index, vendor: 0, device: 0, windows: false }
    }

    fn intel(api: Api, index: usize, windows: bool) -> Candidate {
        Candidate { api, kind: Kind::Integrated, index, vendor: VENDOR_INTEL, device: 0x591b, windows }
    }

    fn info(vendor: u32, device: u32, driver: &str, driver_info: &str) -> Info {
        Info { vendor, device, driver: driver.into(), driver_info: driver_info.into() }
    }

    #[test]
    fn the_window_tries_intel_vulkan_first_and_the_pipeline_tries_dx12_first_on_windows() {
        let list = [intel(Api::Vulkan, 0, true), intel(Api::Dx12, 1, true), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list, Prefer::Unchanged, Role::Window), [0, 2, 1], "the Intel DX12 adapter stays behind every Vulkan one");
        assert_eq!(order(&list, Prefer::Unchanged, Role::Pipeline), [1, 2, 0], "card sharing of decoded pictures needs DX12");
    }

    #[test]
    fn the_window_device_serves_the_pipeline_except_on_intel_vulkan_in_windows() {
        assert!(!window_serves_pipeline(Api::Vulkan, VENDOR_INTEL, true));
        assert!(window_serves_pipeline(Api::Dx12, VENDOR_INTEL, true));
        assert!(window_serves_pipeline(Api::Vulkan, VENDOR_INTEL, false));
        assert!(window_serves_pipeline(Api::Vulkan, 0x10de, true));
    }

    #[test]
    fn intel_elsewhere_keeps_vulkan_first_and_other_vendors_on_windows_go_dx12_first_only_when_asked_for_both_roles() {
        let list = [intel(Api::Dx12, 0, false), intel(Api::Vulkan, 1, false)];
        assert_eq!(order_by(&list, Prefer::Unchanged, Role::Pipeline, false), [1, 0]);
        let nvidia = |api, index| Candidate { api, kind: Kind::Discrete, index, vendor: 0x10de, device: 0x1c20, windows: true };
        let cards = [nvidia(Api::Vulkan, 0), nvidia(Api::Dx12, 1)];
        for role in [Role::Window, Role::Pipeline] {
            assert_eq!(order_by(&cards, Prefer::Unchanged, role, false), [1, 0]);
            assert_eq!(order_by(&cards, Prefer::Unchanged, role, true), [0, 1], "without the request Vulkan is first");
        }
    }

    #[test]
    fn asking_for_dx12_first_never_moves_intel() {
        for vulkan_first in [false, true] {
            for prefer in [Prefer::Unchanged, Prefer::HighPerformance, Prefer::PowerSaving] {
                let list = [intel(Api::Vulkan, 0, true), intel(Api::Dx12, 1, true)];
                assert_eq!(order_by(&list, prefer, Role::Window, vulkan_first)[0], 0, "Intel's window stays on Vulkan");
                assert_eq!(order_by(&list, prefer, Role::Pipeline, vulkan_first)[0], 1, "Intel's pipeline stays on DX12");
            }
        }
        let linux = [c(Api::Dx12, Kind::Discrete, 0), c(Api::Vulkan, Kind::Discrete, 1)];
        assert_eq!(order_by(&linux, Prefer::Unchanged, Role::Window, true), order_by(&linux, Prefer::Unchanged, Role::Window, false), "nothing changes off Windows");
    }

    #[test]
    fn only_a_saved_dx12_choice_asks_for_dx12_first() {
        assert!(dx12_first_for("dx12") && dx12_first_for(" dx12 "));
        assert!(!dx12_first_for("") && !dx12_first_for("vulkan") && !dx12_first_for("nonsense"));
    }

    #[test]
    fn the_windows_card_goes_dx12_first_on_other_vendors_when_asked() {
        let gtx = |api, index| Candidate { api, kind: Kind::Discrete, index, vendor: 0x10de, device: 0x1c20, windows: true };
        let list = [gtx(Api::Vulkan, 0), intel(Api::Vulkan, 1, true), intel(Api::Dx12, 2, true), gtx(Api::Dx12, 3)];
        assert_eq!(order_by(&list, Prefer::HighPerformance, Role::Window, false), [3, 0, 1, 2]);
        assert_eq!(order_by(&list, Prefer::HighPerformance, Role::Pipeline, false), [3, 0, 2, 1]);
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
        assert_eq!(order(&list, Prefer::Unchanged, Role::Window), [3, 1, 2, 0]);
    }

    #[test]
    fn the_drivers_order_among_real_gpus_stays() {
        let list = [c(Api::Vulkan, Kind::Integrated, 0), c(Api::Vulkan, Kind::Discrete, 1)];
        assert_eq!(order(&list, Prefer::Unchanged, Role::Window), [0, 1]);
        let list = [c(Api::Vulkan, Kind::Discrete, 0), c(Api::Vulkan, Kind::Integrated, 1)];
        assert_eq!(order(&list, Prefer::Unchanged, Role::Window), [0, 1]);
    }

    #[test]
    fn vulkan_goes_before_dx12() {
        let list = [c(Api::Dx12, Kind::Integrated, 0), c(Api::Dx12, Kind::Discrete, 1), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list, Prefer::Unchanged, Role::Window), [2, 0, 1]);
    }

    #[test]
    fn a_real_vulkan_gpu_beats_a_dx12_software_adapter() {
        let list = [c(Api::Dx12, Kind::Cpu, 0), c(Api::Vulkan, Kind::Integrated, 1)];
        assert_eq!(order(&list, Prefer::Unchanged, Role::Window), [1, 0]);
    }

    fn laptop() -> [Candidate; 4] {
        let gtx = |api, index| Candidate { api, kind: Kind::Discrete, index, vendor: 0x10de, device: 0x1c20, windows: true };
        [gtx(Api::Vulkan, 0), intel(Api::Vulkan, 1, true), intel(Api::Dx12, 2, true), gtx(Api::Dx12, 3)]
    }

    #[test]
    fn high_performance_puts_the_discrete_card_before_intel() {
        assert_eq!(order_by(&laptop(), Prefer::HighPerformance, Role::Window, true), [0, 3, 1, 2]);
        assert_eq!(order_by(&laptop(), Prefer::HighPerformance, Role::Pipeline, true), [0, 3, 2, 1]);
    }

    #[test]
    fn power_saving_puts_intel_before_the_discrete_card() {
        assert_eq!(order_by(&laptop(), Prefer::PowerSaving, Role::Window, true), [1, 2, 0, 3]);
        assert_eq!(order_by(&laptop(), Prefer::PowerSaving, Role::Pipeline, true), [2, 1, 0, 3]);
    }

    #[test]
    fn unchanged_keeps_the_old_order() {
        assert_eq!(order_by(&laptop(), Prefer::Unchanged, Role::Window, true), [0, 1, 2, 3]);
        assert_eq!(order_by(&laptop(), Prefer::Unchanged, Role::Pipeline, true), [0, 2, 1, 3]);
    }

    #[test]
    fn a_preference_still_keeps_software_cards_last() {
        let list = [c(Api::Vulkan, Kind::Cpu, 0), c(Api::Vulkan, Kind::Integrated, 1), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order_by(&list, Prefer::HighPerformance, Role::Window, true), [2, 1, 0]);
        assert_eq!(order_by(&list, Prefer::PowerSaving, Role::Window, true), [1, 2, 0]);
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
        let ordered = order_by(&laptop(), Prefer::HighPerformance, Role::Window, true);
        assert_eq!(ordered, [0, 3, 1, 2]);
        assert_eq!(with_choice(ordered.clone(), &laptop(), "8086:591b"), [1, 2, 0, 3], "both Intel adapters first, Vulkan before DX12");
        assert_eq!(with_choice(ordered.clone(), &laptop(), "10DE:1C20"), ordered);
    }

    #[test]
    fn a_chosen_intel_card_keeps_dx12_first_for_the_pipeline() {
        let ordered = order_by(&laptop(), Prefer::HighPerformance, Role::Pipeline, true);
        assert_eq!(with_choice(ordered, &laptop(), "8086:591b"), [2, 1, 0, 3]);
    }

    #[test]
    fn the_pipeline_follows_the_windows_card_between_two_discrete_cards() {
        let card = |api, index, vendor| Candidate { api, kind: Kind::Discrete, index, vendor, device: 0x1, windows: true };
        let list = [card(Api::Vulkan, 0, VENDOR_INTEL), card(Api::Vulkan, 1, 0x10de), card(Api::Dx12, 2, VENDOR_INTEL), card(Api::Dx12, 3, 0x10de)];
        let ordered = order_by(&list, Prefer::HighPerformance, Role::Pipeline, true);
        assert_eq!(ordered, [1, 2, 0, 3], "alone, the other vendor's Vulkan adapter sorts before Intel's DX12 one");
        assert_eq!(with_choice(ordered, &list, &card_id(VENDOR_INTEL, 0x1)), [2, 0, 1, 3], "the window's card goes first");
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
        assert_eq!(listed(), [Listed { id: "10de:1c20".into(), name: "GTX".into() }, Listed { id: "8086:591b".into(), name: "Intel (R)".into() }]);
    }

    #[test]
    fn one_adapter_can_be_moved_first() {
        let list = laptop();
        assert_eq!(move_to_front(vec![2, 0, 3], &list, 3), [0, 3, 2], "the whole card moves, in its own order");
        assert_eq!(move_to_front(vec![2, 0, 3], &list, 9), [2, 0, 3]);
        assert_eq!(move_to_front(vec![2, 1, 0, 3], &list, 1), [2, 1, 0, 3], "picking Intel's Vulkan adapter keeps the pipeline's DX12 first");
        let soft = [c(Api::Vulkan, Kind::Discrete, 0), c(Api::Vulkan, Kind::Cpu, 1)];
        assert_eq!(move_to_front(vec![0, 1], &soft, 1), [0, 1], "software never goes first");
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(order(&[], Prefer::Unchanged, Role::Window).is_empty());
    }
}
