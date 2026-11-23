//! A graphics driver that crashes the whole program can't be caught, so before a card is opened it goes in a small file, and the
//! file is removed once the window has shown. A file still there at the next start means that card (with that driver version) took
//! the last start down, and it is left out of the order for a while. The wait grows each time the card fails again, so a driver that really
//! crashes stays out of the way and a card that was only unlucky (the system killed Telescope when memory ran out) comes back.
//! Both files sit in the settings folder, a dev profile's own.

use crate::gpu_pick::has_version;
use crate::i18n::{tr, Fields};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The card being opened, as the log and the skip list name it. A new driver version is a different card, so it gets another try.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub name: String,
    pub backend: String,
    pub driver: String,
    /// The card's PCI bus address when the driver gives one, so two cards of one model are told apart. Empty when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bus: String,
}

impl Card {
    /// Whether `self`, a card on the skip list, is this card. An old entry written without a driver version (the driver said none) still
    /// matches the same card once a version is found for it.
    pub fn is_listed_as(&self, listed: &Card) -> bool {
        self.name == listed.name && self.backend == listed.backend
            && (self.bus.is_empty() || listed.bus.is_empty() || self.bus == listed.bus)
            && (self.driver == listed.driver || (!has_version(&listed.driver) && self.driver.strip_prefix(listed.driver.as_str()).is_some_and(|rest| rest.starts_with(' '))))
    }

    pub fn new(name: &str, backend: &str, driver: &str) -> Card {
        Card { name: name.trim().into(), backend: backend.into(), driver: driver.trim().into(), bus: String::new() }
    }

    /// This card at a PCI bus address (empty leaves it unknown).
    pub fn with_bus(mut self, bus: &str) -> Card {
        self.bus = bus.trim().into();
        self
    }
}

impl std::fmt::Display for Card {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}, driver {})", self.name, self.backend, self.driver)
    }
}

/// The marker: the card about to be opened.
pub const MARKER_FILE: &str = "gpu_trying.json";
/// The cards the last starts closed on, oldest first.
pub const SKIP_FILE: &str = "gpu_skipped.json";
/// More than this and the oldest is forgotten, so the list can't grow without end.
const MAX_SKIPPED: usize = 8;
/// A skipped card is tried again after this many starts, or the time below, whichever comes first.
const WAIT_STARTS: u32 = 3;
const WAIT_SECS: u64 = 24 * 60 * 60;
/// Each failure in a row doubles the wait, this many times at most (48 starts, 16 days).
const MAX_DOUBLINGS: u32 = 4;

fn one() -> u32 {
    1
}

/// A card on the skip list. The extra fields are optional in the file: an entry from before the wait existed is one failure that
/// starts counting from the first start that reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    #[serde(flatten)]
    pub card: Card,
    /// How many starts in a row ended on this card.
    #[serde(default = "one")]
    pub strikes: u32,
    /// Starts since the last failure.
    #[serde(default)]
    pub starts: u32,
    /// When the last failure was found, in seconds since 1970 (0 when unknown).
    #[serde(default)]
    pub since: u64,
}

impl Skipped {
    fn new(card: Card, now: u64) -> Skipped {
        Skipped { card, strikes: 1, starts: 0, since: now }
    }

    /// Whether the wait is over and the card gets another try. A clock set back never ends it early.
    fn due(&self, now: u64) -> bool {
        let doublings = self.strikes.saturating_sub(1).min(MAX_DOUBLINGS);
        self.starts >= WAIT_STARTS << doublings || (self.since != 0 && now.saturating_sub(self.since) >= WAIT_SECS << doublings)
    }
}

/// The skip list after a start, `now` in seconds since 1970. This start counts for every card that wasn't what the last one closed on.
/// The card the last start closed on (the marker) is added, or has one more failure and a fresh wait if it was on the list already
/// (it was given another try). The oldest go when there are too many. The card is what the last start closed on, if anything.
pub fn after_start(marker: Option<Card>, mut skipped: Vec<Skipped>, now: u64) -> (Vec<Skipped>, Option<Card>) {
    let hit = marker.as_ref().and_then(|card| skipped.iter().position(|s| card.is_listed_as(&s.card)));
    for (i, s) in skipped.iter_mut().enumerate() {
        if s.since == 0 {
            s.since = now;
        }
        if Some(i) != hit {
            s.starts = s.starts.saturating_add(1);
        }
    }
    let Some(card) = marker else { return (skipped, None) };
    match hit {
        Some(i) => {
            let s = &mut skipped[i];
            (s.strikes, s.starts, s.since) = (s.strikes.saturating_add(1), 0, now);
        }
        None => skipped.push(Skipped::new(card.clone(), now)),
    }
    let extra = skipped.len().saturating_sub(MAX_SKIPPED);
    skipped.drain(..extra);
    (skipped, Some(card))
}

/// The cards still to be left out: the ones whose wait isn't over.
fn left_out(skipped: &[Skipped], now: u64) -> Vec<Card> {
    skipped.iter().filter(|s| !s.due(now)).map(|s| s.card.clone()).collect()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What is left of a list of adapters after the skipped cards go, in the same order.
pub fn without_skipped<T>(list: Vec<T>, card_of: impl Fn(&T) -> Card, skipped: &[Card]) -> Vec<T> {
    list.into_iter().filter(|item| !skipped.iter().any(|s| card_of(item).is_listed_as(s))).collect()
}

#[derive(Default)]
struct State {
    /// Markers are only written while a window start is under way, so commands and tests that open a card leave nothing behind.
    folder: Option<PathBuf>,
    /// The folder of the start, kept after the marker is cleared so a deferred start can arm it again.
    home: Option<PathBuf>,
    /// The card last marked.
    last: Option<Card>,
    /// A start in the tray: no surface exists yet, so the marker was cleared and is armed again when the window first shows.
    deferred: bool,
    /// The cards left out of this start.
    skipped: Vec<Card>,
    /// The skip list as the file has it, the cards given another try this start included.
    entries: Vec<Skipped>,
    /// The cards given another try this start: one that draws the window comes off the list (`confirmed`).
    on_trial: Vec<Card>,
    /// Goes up with every `clear`, so work that marked something meanwhile can tell it was cleared underneath.
    generation: u64,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: Mutex<State> = Mutex::new(State { folder: None, home: None, last: None, deferred: false, skipped: Vec::new(), entries: Vec::new(), on_trial: Vec::new(), generation: 0 });
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Writes a file whole or not at all: to a name beside it first, then moved over.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Reads what the last start left, in `folder`. The card it closed on (if any) is logged, skipped for a while and returned, so the
/// window can say so once. A card whose wait is over is tried again. Until `clear` is called, `mark` writes the marker.
pub fn begin_start(folder: &Path) -> Option<Card> {
    begin_start_at(folder, unix_now())
}

fn begin_start_at(folder: &Path, now: u64) -> Option<Card> {
    let marker = read::<Card>(&folder.join(MARKER_FILE));
    let (entries, crashed) = after_start(marker, read(&folder.join(SKIP_FILE)).unwrap_or_default(), now);
    if let Some(card) = &crashed {
        let strikes = entries.iter().find(|s| card.is_listed_as(&s.card)).map_or(1, |s| s.strikes);
        log::error!("The last start closed while opening the graphics card {card} (failure {strikes} in a row), so it is skipped for a while");
    }
    if crashed.is_some() || !entries.is_empty() {
        let saved = save_entries(folder, &entries);
        if let Err(e) = &saved {
            log::warn!("Couldn't save the skipped graphics cards: {e}");
        }
        // The marker goes only once the list is saved: losing both would let the card be tried again at once
        if crashed.is_some() && saved.is_ok() {
            let _ = std::fs::remove_file(folder.join(MARKER_FILE));
        }
    }
    let on_trial: Vec<Card> = entries.iter().filter(|s| s.due(now)).map(|s| s.card.clone()).collect();
    for card in &on_trial {
        log::info!("The graphics card {card} was skipped after a start that closed on it, and gets another try");
    }
    let mut state = state();
    state.folder = Some(folder.to_path_buf());
    state.home = Some(folder.to_path_buf());
    state.deferred = false;
    state.skipped = left_out(&entries, now);
    state.entries = entries;
    state.on_trial = on_trial;
    crashed
}

fn save_entries(folder: &Path, entries: &[Skipped]) -> Result<(), String> {
    let text = serde_json::to_string(entries).map_err(|e| e.to_string())?;
    write_atomic(&folder.join(SKIP_FILE), &text).map_err(|e| e.to_string())
}

/// The window drew on the card it opened, so a card that was given another try and is that one is no longer skipped, and its failures are
/// forgotten. When the card probe was given another try, any card that draws the window clears it.
pub fn confirmed() {
    let mut state = state();
    let Some(home) = state.home.clone() else { return };
    let (trial, last) = (state.on_trial.clone(), state.last.clone());
    let cleared = |s: &Skipped| trial.contains(&s.card) && (s.card.name == PROBE_NAME || last.as_ref().is_some_and(|l| l.is_listed_as(&s.card)));
    let kept: Vec<Skipped> = state.entries.iter().filter(|s| !cleared(s)).cloned().collect();
    if kept.len() == state.entries.len() {
        return;
    }
    state.on_trial.retain(|c| kept.iter().any(|s| &s.card == c));
    state.entries = kept;
    if let Err(e) = save_entries(&home, &state.entries) {
        log::warn!("Couldn't save the skipped graphics cards: {e}");
    }
}

/// Whether the card is on the skip list (always false before `begin_start`).
pub fn is_skipped(card: &Card) -> bool {
    state().skipped.iter().any(|s| card.is_listed_as(s))
}

/// The skip list as it stands.
pub fn skipped() -> Vec<Card> {
    state().skipped.clone()
}

/// What to do after a frame was drawn while the start waits to be confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Not yet proven: ask for another picture.
    AskForAnother,
    /// Enough pictures went out: the card is cleared.
    Confirm,
    Nothing,
}

/// Counts the window's drawn frames. Slint says a frame is drawn before it is presented, and a card that can't present to the window's
/// surface fails exactly there, with nothing to say a present worked. So the start counts as done after a few frames over about a second.
#[derive(Default)]
pub struct Confirm {
    frames: u32,
    first_ms: u64,
    done: bool,
}

const CONFIRM_FRAMES: u32 = 3;
const CONFIRM_MS: u64 = 1000;

impl Confirm {
    pub fn frame(&mut self, now_ms: u64) -> Frame {
        if self.done {
            return Frame::Nothing;
        }
        self.frames += 1;
        if self.frames == 1 {
            self.first_ms = now_ms;
        }
        if self.frames >= CONFIRM_FRAMES && now_ms.saturating_sub(self.first_ms) >= CONFIRM_MS {
            self.done = true;
            return Frame::Confirm;
        }
        Frame::AskForAnother
    }
}

/// Reads the skip list from `folder` without starting a window start: for commands that open a card but never mark one (snapshot).
/// Nothing is counted or saved.
pub fn load_skipped(folder: &Path) {
    let entries: Vec<Skipped> = read(&folder.join(SKIP_FILE)).unwrap_or_default();
    state().skipped = left_out(&entries, unix_now());
}

static KEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The window failed after its card was opened (it couldn't be shown, or the event loop ended with an error), which can be that card
/// failing to draw: the marker is left for the next start.
pub fn keep() {
    KEPT.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// `clear`, except after `keep`.
pub fn clear_unless_kept() {
    if !KEPT.load(std::sync::atomic::Ordering::SeqCst) {
        clear();
    }
}

/// Whether a panic's text looks like the graphics driver or the renderer failing, so a panic at start isn't cleared as "has its reason".
pub fn panic_blames_card(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    ["wgpu", "vulkan", "swapchain", "surface", "femtovg", "slint-renderer", "slint_renderer", "d3d12", "dxgi", "opengl", "glutin"].iter().any(|w| text.contains(w))
}

/// Notes that this card is about to be opened. Does nothing outside a window start.
pub fn mark(card: &Card) {
    let mut state = state();
    state.last = Some(card.clone());
    let Some(folder) = &state.folder else { return };
    let written = serde_json::to_string(card).map_err(|e| e.to_string())
        .and_then(|text| std::fs::create_dir_all(folder).and_then(|_| std::fs::write(folder.join(MARKER_FILE), text)).map_err(|e| e.to_string()));
    if let Err(e) = written {
        log::warn!("Couldn't note which graphics card is being opened: {e}");
    }
}

/// Marks `card` while `work` runs and puts back what was marked before: the picture work opens its card while the window's may still be
/// unconfirmed, and a crash in the one must not be put on the other.
pub fn marked_while<T>(card: &Card, work: impl FnOnce() -> T) -> T {
    let (folder, last, generation, before) = {
        let state = state();
        let before = state.folder.as_ref().and_then(|folder| read::<Card>(&folder.join(MARKER_FILE)));
        (state.folder.clone(), state.last.clone(), state.generation, before)
    };
    let Some(folder) = folder else { return work() };
    mark(card);
    let done = work();
    // A clear meanwhile (the window confirmed) wins: putting the old marker back would outlive a healthy run
    let now = state();
    if now.generation == generation && now.folder.as_ref() == Some(&folder) {
        let path = folder.join(MARKER_FILE);
        match &before {
            Some(card) => {
                let _ = serde_json::to_string(card).map(|text| write_atomic(&path, &text));
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    drop(now);
    state().last = last;
    done
}

/// The pseudo card named in the marker while wgpu looks for cards: loading drivers and listing adapters can crash too, before any card is
/// opened. Its driver text is the app's version, so an update is allowed to look again.
pub const PROBE_NAME: &str = "graphics card probe";

pub fn probe_card() -> Card {
    Card::new(PROBE_NAME, "probe", env!("CARGO_PKG_VERSION"))
}

/// Whether the last start closed while looking for graphics cards at all: no card is looked for then.
pub fn probe_failed() -> bool {
    is_skipped(&probe_card())
}

/// The start is in the tray, where no surface exists to draw on: nothing can fail yet, so the marker goes until the window first shows.
pub fn defer() {
    clear();
    state().deferred = true;
}

/// The window shows for the first time after a deferred start: the marker is armed again for its first pictures.
pub fn rearm() {
    let mut state = state();
    if !std::mem::take(&mut state.deferred) {
        return;
    }
    let (Some(home), Some(card)) = (state.home.clone(), state.last.clone()) else { return };
    state.folder = Some(home.clone());
    drop(state);
    mark(&card);
}

/// The window has shown (or the start failed with a message): the card didn't take the program down, so the marker goes.
pub fn clear() {
    let mut state = state();
    state.generation += 1;
    if let Some(folder) = state.folder.take() {
        let _ = std::fs::remove_file(folder.join(MARKER_FILE));
    }
}

/// The title and words for the box that says the last start closed on this card.
pub fn crash_text(card: &Card) -> (String, String) {
    if card.name == PROBE_NAME {
        return (
            tr("Telescope switched graphics card"),
            tr("The last start of Telescope closed while it was looking for graphics cards. Telescope now draws on the processor and doesn't look again until it is updated."),
        );
    }
    (
        tr("Telescope switched graphics card"),
        tr("The last start of Telescope closed while it was opening {card}. Telescope now uses another graphics card, or the processor if there is none left.").field("card", &card.name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_dir;

    fn card(name: &str, driver: &str) -> Card {
        Card::new(name, "Vulkan", driver)
    }

    const T0: u64 = 1_800_000_000;
    const DAY: u64 = 24 * 60 * 60;

    /// An entry as a start long ago would have left it, to use in the tests.
    fn entry(card: Card, strikes: u32, starts: u32, since: u64) -> Skipped {
        Skipped { card, strikes, starts, since }
    }

    /// A card that stays skipped through any start the tests make.
    fn long_skipped(card: Card) -> Skipped {
        entry(card, MAX_DOUBLINGS + 1, 0, u64::MAX)
    }

    /// How many starts, a minute apart, until the card is given another try.
    fn starts_until_due(mut list: Vec<Skipped>) -> u32 {
        for n in 1..1000 {
            list = after_start(None, list, T0).0;
            if list[0].due(T0) {
                return n;
            }
        }
        panic!("never due");
    }

    #[test]
    fn no_marker_only_counts_the_start() {
        let (list, crashed) = after_start(None, vec![entry(card("A", "1"), 1, 0, T0)], T0);
        assert_eq!((list, crashed), (vec![entry(card("A", "1"), 1, 1, T0)], None));
    }

    #[test]
    fn a_marker_puts_its_card_on_the_list_once() {
        let (list, crashed) = after_start(Some(card("A", "1")), vec![entry(card("B", "1"), 1, 0, T0)], T0);
        assert_eq!((list.clone(), crashed), (vec![entry(card("B", "1"), 1, 1, T0), entry(card("A", "1"), 1, 0, T0)], Some(card("A", "1"))));
        // The same card again is the same entry, with another failure
        let (again, crashed) = after_start(Some(card("A", "1")), list, T0 + 5);
        assert_eq!((again.len(), again[1].strikes, again[1].since, crashed), (2, 2, T0 + 5, Some(card("A", "1"))));
    }

    #[test]
    fn the_list_stays_small_and_forgets_the_oldest() {
        let full: Vec<Skipped> = (0..MAX_SKIPPED).map(|i| entry(card(&format!("C{i}"), "1"), 1, 0, T0)).collect();
        let (list, _) = after_start(Some(card("New", "1")), full, T0);
        assert_eq!((list.len(), &list[0].card, list.last().map(|s| &s.card)), (MAX_SKIPPED, &card("C1", "1"), Some(&card("New", "1"))));
    }

    #[test]
    fn a_skipped_card_comes_back_after_a_few_starts_and_waits_longer_each_time_it_fails() {
        let mut list = after_start(Some(card("A", "1")), vec![], T0).0;
        assert!(!list[0].due(T0));
        let mut waits = vec![];
        for _ in 0..7 {
            waits.push(starts_until_due(list.clone()));
            // Given another try, and it fails again
            list = after_start(Some(card("A", "1")), list, T0).0;
        }
        // Doubling, up to a limit
        assert_eq!(waits, [3, 6, 12, 24, 48, 48, 48]);
        assert_eq!(list[0].strikes, 8);
    }

    #[test]
    fn a_skipped_card_comes_back_after_a_day_even_without_starts_and_a_repeat_waits_longer() {
        let list = after_start(Some(card("A", "1")), vec![], T0).0;
        assert!(!list[0].due(T0 + DAY - 1) && list[0].due(T0 + DAY));
        let twice = after_start(Some(card("A", "1")), list, T0 + DAY).0;
        assert!(!twice[0].due(T0 + 3 * DAY - 1) && twice[0].due(T0 + 3 * DAY), "two days after the second failure");
        let capped = entry(card("A", "1"), 99, 0, T0);
        assert!(!capped.due(T0 + 16 * DAY - 1) && capped.due(T0 + 16 * DAY));
    }

    #[test]
    fn a_clock_set_back_or_an_unknown_time_never_ends_the_wait_early() {
        assert!(!entry(card("A", "1"), 1, 0, T0).due(T0 - 10 * DAY));
        assert!(!entry(card("A", "1"), 1, 0, 0).due(T0), "no time known: only starts count");
        assert!(entry(card("A", "1"), 1, 3, 0).due(T0));
    }

    #[test]
    fn an_entry_from_before_the_wait_existed_starts_waiting_at_its_first_start() {
        let old: Vec<Skipped> = serde_json::from_str(r#"[{"name":"A","backend":"Vulkan","driver":"1"},{"name":"B","backend":"Vulkan","driver":"2","bus":"0000:01:00.0"}]"#).unwrap();
        assert_eq!(old[0], entry(card("A", "1"), 1, 0, 0));
        assert_eq!(old[1].card.bus, "0000:01:00.0");
        let (list, _) = after_start(None, old, T0);
        assert!(list.iter().all(|s| s.since == T0 && !s.due(T0)), "{list:?}");
        // What is written is read back the same
        assert_eq!(serde_json::from_str::<Vec<Skipped>>(&serde_json::to_string(&list).unwrap()).unwrap(), list);
    }

    #[test]
    fn a_new_driver_version_is_another_card() {
        let skipped = [card("A", "21.5.2")];
        assert_eq!(without_skipped(vec![card("A", "21.5.2"), card("A", "22.1.0"), card("B", "21.5.2")], |c| c.clone(), &skipped), [card("A", "22.1.0"), card("B", "21.5.2")]);
    }

    #[test]
    fn an_old_entry_without_a_version_still_skips_the_card_once_a_version_is_found() {
        let skipped = [card("A", "Intel driver")];
        let list = vec![card("A", "Intel driver 31.0.101.2141"), card("A", "Other 31.0.101.2141"), card("A", "Intel driver"), card("B", "Intel driver 1.2")];
        assert_eq!(without_skipped(list, |c| c.clone(), &skipped), [card("A", "Other 31.0.101.2141"), card("B", "Intel driver 1.2")]);
        let (list, _) = after_start(Some(card("A", "Intel driver 31.0.101.2141")), vec![entry(skipped[0].clone(), 1, 0, T0)], T0);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn an_entry_with_a_version_does_not_match_a_newer_one_that_starts_the_same() {
        let skipped = [card("A", "Intel driver 31.0.1")];
        assert_eq!(without_skipped(vec![card("A", "Intel driver 31.0.1 b")], |c| c.clone(), &skipped).len(), 1);
    }

    #[test]
    fn a_start_is_confirmed_by_a_few_frames_over_a_second() {
        let mut c = Confirm::default();
        assert_eq!((c.frame(0), c.frame(16), c.frame(32)), (Frame::AskForAnother, Frame::AskForAnother, Frame::AskForAnother), "three quick frames aren't enough");
        assert_eq!((c.frame(1000), c.frame(1016), c.frame(1032)), (Frame::Confirm, Frame::Nothing, Frame::Nothing));
        let mut slow = Confirm::default();
        assert_eq!((slow.frame(0), slow.frame(600), slow.frame(1500)), (Frame::AskForAnother, Frame::AskForAnother, Frame::Confirm));
    }

    #[test]
    fn twins_on_different_buses_are_different_cards() {
        let a = Card::new("A", "Vulkan", "1").with_bus("0000:01:00.0");
        let b = Card::new("A", "Vulkan", "1").with_bus("0000:02:00.0");
        let unknown = Card::new("A", "Vulkan", "1");
        assert!(!b.is_listed_as(&a) && a.is_listed_as(&a));
        assert!(unknown.is_listed_as(&a) && a.is_listed_as(&unknown), "an entry without a bus still matches");
        assert_eq!(without_skipped(vec![a.clone(), b.clone()], |c| c.clone(), std::slice::from_ref(&a)), [b]);
    }

    #[test]
    fn a_driver_or_renderer_panic_blames_the_card() {
        assert!(panic_blames_card("panicked at wgpu-core/src/present.rs: Surface configuration failed"));
        assert!(panic_blames_card("failed to create swapchain"));
        assert!(!panic_blames_card("panicked at crates/shell/src/shell.rs:10: index out of bounds"));
    }

    #[test]
    fn the_backend_is_part_of_the_card() {
        let skipped = [Card::new("A", "Vulkan", "1")];
        let list = vec![Card::new("A", "Vulkan", "1"), Card::new("A", "Dx12", "1")];
        assert_eq!(without_skipped(list, |c| c.clone(), &skipped), [Card::new("A", "Dx12", "1")]);
    }

    // One test for the files and the shared state, since they are the same process-wide state
    #[test]
    fn a_marker_left_behind_is_a_skip_next_time_and_clear_removes_it() {
        let folder = scratch_dir("gpu-marker");
        // Nothing marks before a start begins
        mark(&card("A", "1"));
        assert!(!folder.join(MARKER_FILE).exists());
        assert_eq!(begin_start(&folder), None);
        mark(&card("A", "1"));
        assert!(folder.join(MARKER_FILE).exists());
        // The start closed without clear: the next one finds the marker
        assert_eq!(begin_start(&folder), Some(card("A", "1")));
        assert!(is_skipped(&card("A", "1")) && !is_skipped(&card("A", "2")));
        assert!(!folder.join(MARKER_FILE).exists());
        // The picture work's card is marked while it opens, and the window's marker is back afterwards
        mark(&card("C", "1"));
        assert_eq!(marked_while(&card("D", "1"), || read::<Card>(&folder.join(MARKER_FILE))), Some(card("D", "1")));
        assert_eq!(read::<Card>(&folder.join(MARKER_FILE)), Some(card("C", "1")));
        // The window confirms while the picture work is inside: the old marker isn't put back
        marked_while(&card("D", "1"), clear);
        assert!(!folder.join(MARKER_FILE).exists());
        begin_start(&folder);
        mark(&card("C", "1"));
        clear();
        assert_eq!(marked_while(&card("D", "1"), || ()), ());
        assert!(!folder.join(MARKER_FILE).exists());
        // A tray start has no marker until its window first shows, and a normal start isn't armed again
        begin_start(&folder);
        mark(&card("T", "1"));
        defer();
        assert!(!folder.join(MARKER_FILE).exists());
        rearm();
        assert_eq!(read::<Card>(&folder.join(MARKER_FILE)), Some(card("T", "1")));
        clear();
        rearm();
        assert!(!folder.join(MARKER_FILE).exists());
        // Looking for cards is marked as its own pseudo card, and skipped from then on
        begin_start(&folder);
        mark(&probe_card());
        assert_eq!(begin_start(&folder), Some(probe_card()));
        assert!(probe_failed());
        std::fs::write(folder.join(SKIP_FILE), serde_json::to_string(&[long_skipped(card("A", "1"))]).unwrap()).unwrap();
        begin_start(&folder);
        assert!(!probe_failed());
        clear();
        begin_start(&folder);
        // The skip survives another start that went fine
        mark(&card("B", "1"));
        clear();
        assert!(!folder.join(MARKER_FILE).exists());
        assert_eq!(begin_start(&folder), None);
        assert_eq!(skipped(), vec![card("A", "1")]);
        // A failure after the card opened keeps the marker, any other clears it
        mark(&card("E", "1"));
        keep();
        clear_unless_kept();
        assert!(folder.join(MARKER_FILE).exists());
        KEPT.store(false, std::sync::atomic::Ordering::SeqCst);
        clear_unless_kept();
        assert!(!folder.join(MARKER_FILE).exists());
        // A command that marks nothing still reads the list
        std::fs::write(folder.join(SKIP_FILE), serde_json::to_string(&[long_skipped(card("S", "9"))]).unwrap()).unwrap();
        load_skipped(&folder);
        assert!(is_skipped(&card("S", "9")));
        std::fs::write(folder.join(SKIP_FILE), serde_json::to_string(&[long_skipped(card("A", "1"))]).unwrap()).unwrap();
        load_skipped(&folder);
        // A damaged file is ignored
        std::fs::write(folder.join(MARKER_FILE), "not json").unwrap();
        assert_eq!(begin_start(&folder), None);
        clear();
        // A skipped card is tried again after a few starts, and waits longer when that fails too
        let folder = scratch_dir("gpu-marker-expiry");
        let file = || read::<Vec<Skipped>>(&folder.join(SKIP_FILE)).unwrap();
        begin_start_at(&folder, T0);
        mark(&card("X", "1"));
        assert_eq!(begin_start_at(&folder, T0 + 10), Some(card("X", "1")));
        assert!(is_skipped(&card("X", "1")));
        for n in 1..3 {
            begin_start_at(&folder, T0 + 10 + n);
            assert!(is_skipped(&card("X", "1")), "start {n} after the failure");
        }
        begin_start_at(&folder, T0 + 13);
        assert!(!is_skipped(&card("X", "1")) && skipped().is_empty(), "the third start gives it another try");
        assert_eq!(file().len(), 1, "it stays on the list until it has drawn the window");
        // Too early a confirmation by another card doesn't clear it
        mark(&card("Y", "1"));
        confirmed();
        assert_eq!(file().len(), 1);
        clear();
        // Another failure: skipped again, and for twice as long
        begin_start_at(&folder, T0 + 14);
        assert!(!is_skipped(&card("X", "1")));
        mark(&card("X", "1"));
        assert_eq!(begin_start_at(&folder, T0 + 20), Some(card("X", "1")));
        assert_eq!((file()[0].strikes, file()[0].starts), (2, 0));
        for n in 1..6 {
            begin_start_at(&folder, T0 + 20 + n);
            assert!(is_skipped(&card("X", "1")), "start {n} after the second failure");
        }
        begin_start_at(&folder, T0 + 26);
        assert!(!is_skipped(&card("X", "1")));
        // This time the card draws the window, so it is forgotten
        mark(&card("X", "1"));
        confirmed();
        clear();
        assert!(file().is_empty());
        begin_start_at(&folder, T0 + 27);
        assert!(skipped().is_empty() && !is_skipped(&card("X", "1")));
        // A day later is another way back, with no starts in between
        mark(&card("Z", "1"));
        begin_start_at(&folder, T0 + 30);
        begin_start_at(&folder, T0 + 30 + DAY - 1);
        assert!(is_skipped(&card("Z", "1")));
        begin_start_at(&folder, T0 + 30 + DAY);
        assert!(!is_skipped(&card("Z", "1")));
        clear();
    }
}
