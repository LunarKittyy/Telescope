//! A graphics driver that crashes the whole program can't be caught, so before a card is opened it goes in a small file, and the
//! file is removed once the window has shown. A file still there at the next start means that card (with that driver version) took
//! the last start down, and it is left out of the order from then on. Both files sit in the settings folder, a dev profile's own.

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
}

impl Card {
    pub fn new(name: &str, backend: &str, driver: &str) -> Card {
        Card { name: name.trim().into(), backend: backend.into(), driver: driver.trim().into() }
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

/// The skip list once a marker was found: the card is added (once), and the oldest go when there are too many. The card is what
/// the last start closed on, if anything.
pub fn after_start(marker: Option<Card>, mut skipped: Vec<Card>) -> (Vec<Card>, Option<Card>) {
    let Some(card) = marker else { return (skipped, None) };
    if !skipped.contains(&card) {
        skipped.push(card.clone());
    }
    let extra = skipped.len().saturating_sub(MAX_SKIPPED);
    skipped.drain(..extra);
    (skipped, Some(card))
}

/// What is left of a list of adapters after the skipped cards go, in the same order.
pub fn without_skipped<T>(list: Vec<T>, card_of: impl Fn(&T) -> Card, skipped: &[Card]) -> Vec<T> {
    list.into_iter().filter(|item| !skipped.contains(&card_of(item))).collect()
}

#[derive(Default)]
struct State {
    /// Markers are only written while a window start is under way, so commands and tests that open a card leave nothing behind.
    folder: Option<PathBuf>,
    skipped: Vec<Card>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: Mutex<State> = Mutex::new(State { folder: None, skipped: Vec::new() });
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Reads what the last start left, in `folder`. The card it closed on (if any) is logged, skipped from now on and returned, so the
/// window can say so once. Until `clear` is called, `mark` writes the marker.
pub fn begin_start(folder: &Path) -> Option<Card> {
    let marker = read::<Card>(&folder.join(MARKER_FILE));
    let (skipped, crashed) = after_start(marker, read(&folder.join(SKIP_FILE)).unwrap_or_default());
    if let Some(card) = &crashed {
        log::error!("The last start closed while opening the graphics card {card}, so it is skipped from now on");
        match serde_json::to_string(&skipped) {
            Ok(text) => {
                if let Err(e) = std::fs::create_dir_all(folder).and_then(|_| std::fs::write(folder.join(SKIP_FILE), text)) {
                    log::warn!("Couldn't save the skipped graphics cards: {e}");
                }
            }
            Err(e) => log::warn!("Couldn't save the skipped graphics cards: {e}"),
        }
        let _ = std::fs::remove_file(folder.join(MARKER_FILE));
    }
    let mut state = state();
    state.folder = Some(folder.to_path_buf());
    state.skipped = skipped;
    crashed
}

/// Whether the card is on the skip list (always false before `begin_start`).
pub fn is_skipped(card: &Card) -> bool {
    state().skipped.contains(card)
}

/// The skip list as it stands.
pub fn skipped() -> Vec<Card> {
    state().skipped.clone()
}

/// Notes that this card is about to be opened. Does nothing outside a window start.
pub fn mark(card: &Card) {
    let state = state();
    let Some(folder) = &state.folder else { return };
    let written = serde_json::to_string(card).map_err(|e| e.to_string())
        .and_then(|text| std::fs::create_dir_all(folder).and_then(|_| std::fs::write(folder.join(MARKER_FILE), text)).map_err(|e| e.to_string()));
    if let Err(e) = written {
        log::warn!("Couldn't note which graphics card is being opened: {e}");
    }
}

/// The window has shown (or the start failed with a message): the card didn't take the program down, so the marker goes.
pub fn clear() {
    let mut state = state();
    if let Some(folder) = state.folder.take() {
        let _ = std::fs::remove_file(folder.join(MARKER_FILE));
    }
}

/// The title and words for the box that says the last start closed on this card.
pub fn crash_text(card: &Card) -> (String, String) {
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

    #[test]
    fn no_marker_changes_nothing() {
        let list = vec![card("A", "1")];
        assert_eq!(after_start(None, list.clone()), (list, None));
    }

    #[test]
    fn a_marker_puts_its_card_on_the_list_once() {
        let (list, crashed) = after_start(Some(card("A", "1")), vec![card("B", "1")]);
        assert_eq!((list.clone(), crashed), (vec![card("B", "1"), card("A", "1")], Some(card("A", "1"))));
        let (again, crashed) = after_start(Some(card("A", "1")), list.clone());
        assert_eq!((again, crashed), (list, Some(card("A", "1"))));
    }

    #[test]
    fn the_list_stays_small_and_forgets_the_oldest() {
        let full: Vec<Card> = (0..MAX_SKIPPED).map(|i| card(&format!("C{i}"), "1")).collect();
        let (list, _) = after_start(Some(card("New", "1")), full);
        assert_eq!((list.len(), &list[0], list.last()), (MAX_SKIPPED, &card("C1", "1"), Some(&card("New", "1"))));
    }

    #[test]
    fn a_new_driver_version_is_another_card() {
        let skipped = [card("A", "21.5.2")];
        assert_eq!(without_skipped(vec![card("A", "21.5.2"), card("A", "22.1.0"), card("B", "21.5.2")], |c| c.clone(), &skipped), [card("A", "22.1.0"), card("B", "21.5.2")]);
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
        // The skip survives another start that went fine
        mark(&card("B", "1"));
        clear();
        assert!(!folder.join(MARKER_FILE).exists());
        assert_eq!(begin_start(&folder), None);
        assert_eq!(skipped(), vec![card("A", "1")]);
        // A damaged file is ignored
        std::fs::write(folder.join(MARKER_FILE), "not json").unwrap();
        assert_eq!(begin_start(&folder), None);
        clear();
    }
}
