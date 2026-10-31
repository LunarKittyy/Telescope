//! The Windows start-up splash (telescope.splash): a small window up while the app starts, with a status line in the person's
//! language. A first start after installing can spend a while before the window shows (Windows checks each new program file
//! as it loads), so the splash says why once it's been up a while; a warm start is gone well before.
//!
//! This is the logic, with the window behind SplashWindow: splash_windows.rs draws one on Windows, and there is none elsewhere.

use std::sync::Arc;
use std::time::Duration;
use telescope_core::i18n::{self, tr};

/// Since launch: the line about the security scan only shows when the start is slow.
pub const WHY_AFTER: Duration = Duration::from_secs(5);
/// Characters: about what fits across the splash in its font.
pub const WRAP_AT: usize = 58;

/// The splash window, as much of it as the text needs. Both calls do nothing once it's closed.
pub trait SplashWindow: Send + Sync {
    fn update_text(&self, text: &str);
    fn close(&self);
}

/// textwrap.fill: the text in lines of at most `width` characters, broken at spaces and after a hyphen between letters, and in the
/// middle of a word longer than a line.
pub fn fill(text: &str, width: usize) -> String {
    let flat: String = text.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).collect();
    let mut chunks: Vec<String> = Vec::new();
    // Runs of spaces and of other characters; a word also ends after a hyphen between two letters
    let mut current = String::new();
    let mut chars = flat.chars().peekable();
    let mut previous: Option<char> = None;
    while let Some(c) = chars.next() {
        if previous.is_some_and(|p| (p == ' ') != (c == ' ')) {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(c);
        if c == '-' && previous.is_some_and(char::is_alphabetic) && chars.peek().is_some_and(|n| n.is_alphabetic()) {
            chunks.push(std::mem::take(&mut current));
            previous = None;
            continue;
        }
        previous = Some(c);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let width = width.max(1);
    let len = |s: &str| s.chars().count();
    let blank = |s: &str| s.chars().all(|c| c == ' ');
    let mut stack: Vec<String> = chunks.into_iter().rev().collect();
    let mut lines: Vec<String> = Vec::new();
    while !stack.is_empty() {
        let mut line: Vec<String> = Vec::new();
        let mut used = 0;
        // A line after the first doesn't start with a space
        if !lines.is_empty() && stack.last().is_some_and(|c| blank(c)) {
            stack.pop();
        }
        while let Some(next) = stack.last() {
            if used + len(next) > width {
                break;
            }
            used += len(next);
            line.push(stack.pop().unwrap_or_default());
        }
        if stack.last().is_some_and(|c| len(c) > width) {
            // A word longer than a line fills what's left of this one
            let long = stack.pop().unwrap_or_default();
            let room = width - used;
            let head: String = long.chars().take(room).collect();
            let tail: String = long.chars().skip(room).collect();
            line.push(head);
            stack.push(tail);
        }
        if line.last().is_some_and(|c| blank(c)) {
            line.pop();
        }
        if !line.is_empty() {
            lines.push(line.concat());
        }
    }
    lines.join("\n")
}

/// Puts the status text on the splash in the person's language, before the rest loads (most of the wait). None is no splash (not the
/// packaged build): nothing happens, and the chosen language isn't even asked for. chosen_language is the one picked in Settings; since_launch
/// is how long ago Windows started this process; why_after is how long a start has to take before the second line shows.
pub fn show_text(splash: Option<Arc<dyn SplashWindow>>, chosen_language: impl FnOnce() -> String, since_launch: Duration, why_after: Duration) {
    let Some(splash) = splash else { return };
    let env = std::env::var(i18n::ENV_LANGUAGE).unwrap_or_default();
    let chosen = if env.trim().is_empty() { chosen_language() } else { env.trim().to_string() };
    // The language picked, else the first of Windows' display languages that Telescope has
    i18n::load(Some(&i18n::lang::resolve(&chosen, &i18n::available())));
    let starting = tr("Starting Telescope…");
    // Translators: added under "Starting Telescope…" while the app starts on Windows. The scan is Windows checking new program files
    let why = tr("This can take a minute while Windows runs a security scan.");
    let both = format!("{starting}\n{}", fill(&why, WRAP_AT));
    let wait = why_after.saturating_sub(since_launch);
    if wait.is_zero() {
        splash.update_text(&both);
        return;
    }
    splash.update_text(&starting);
    std::thread::Builder::new().name("splash-text".into()).spawn(move || {
        std::thread::sleep(wait);
        splash.update_text(&both);
    }).ok();
}

/// Title and text of the box said when it's opened from inside the zip, before anything can load (the launcher shows it in
/// every language, scripts/build_bootloader.py).
pub fn extract_first() -> (String, String) {
    // Translators: title of a message box shown when Telescope is opened straight from its zip without extracting it
    let title = tr("Extract Telescope first");
    let text = tr("Telescope can't start from inside the zip.\n\nRight-click the zip, choose Extract All, and open Telescope from the extracted folder.");
    (title, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The pyi_splash stand-in: every text it was given, and whether it was closed.
    #[derive(Default)]
    struct Fake {
        texts: Mutex<Vec<String>>,
        closed: Mutex<bool>,
    }

    impl SplashWindow for Fake {
        fn update_text(&self, text: &str) {
            self.texts.lock().unwrap().push(text.to_string());
        }
        fn close(&self) {
            *self.closed.lock().unwrap() = true;
        }
    }

    const ENGLISH_BOTH: &str = "Starting Telescope…\nThis can take a minute while Windows runs a security scan.";

    // The language is global, so the tests that load one share this
    static LANGUAGE: Mutex<()> = Mutex::new(());

    fn swedish_catalog() {
        let dir = telescope_core::testing::scratch_dir("splash");
        std::fs::write(dir.join("sv.po"), "msgid \"\"\nmsgstr \"\"\n\"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n\nmsgid \"Starting Telescope…\"\nmsgstr \"Startar Telescope…\"\n\n\
            msgid \"This can take a minute while Windows runs a security scan.\"\nmsgstr \"Det kan ta en minut medan Windows söker igenom filerna efter virus.\"\n").unwrap();
        i18n::use_folder(dir);
    }

    // test_splash.py::test_the_splash_text_is_in_the_chosen_language
    #[test]
    fn the_splash_text_is_in_the_chosen_language() {
        let _l = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(i18n::ENV_LANGUAGE);
        swedish_catalog();
        let fake = Arc::new(Fake::default());
        show_text(Some(fake.clone()), || "sv".into(), Duration::from_secs(10), WHY_AFTER);
        let texts = fake.texts.lock().unwrap().clone();
        assert!(texts.first().is_some_and(|t| t.contains("Telescope")), "{texts:?}");
        assert_ne!(texts[0], ENGLISH_BOTH);
        assert_eq!(texts[0], "Startar Telescope…\nDet kan ta en minut medan Windows söker igenom filerna\nefter virus.");
        i18n::load(Some("en"));
    }

    // test_splash.py::test_without_the_packaged_build_nothing_happens
    #[test]
    fn without_the_packaged_build_nothing_happens() {
        let asked = Mutex::new(false);
        show_text(None, || {
            *asked.lock().unwrap() = true;
            "sv".into()
        }, Duration::ZERO, WHY_AFTER);
        assert!(!*asked.lock().unwrap());
    }

    // test_splash.py::test_the_line_about_the_scan_waits_until_the_start_is_slow
    #[test]
    fn the_line_about_the_scan_waits_until_the_start_is_slow() {
        let _l = LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(i18n::ENV_LANGUAGE);
        i18n::load(Some("en"));
        let fake = Arc::new(Fake::default());
        show_text(Some(fake.clone()), || "en".into(), Duration::ZERO, Duration::from_millis(50));
        assert_eq!(*fake.texts.lock().unwrap(), ["Starting Telescope…"]);
        for _ in 0..100 {
            if fake.texts.lock().unwrap().len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fake.texts.lock().unwrap()[1], ENGLISH_BOTH);
    }

    #[test]
    fn the_box_for_a_start_from_inside_the_zip_has_its_words() {
        let (title, text) = extract_first();
        assert_eq!(title, "Extract Telescope first");
        assert!(text.starts_with("Telescope can't start from inside the zip.\n\nRight-click the zip"));
    }

    // What Python's textwrap.fill makes of the same text
    #[test]
    fn text_wraps_like_textwrap_does() {
        assert_eq!(fill("This can take a minute while Windows runs a security scan.", WRAP_AT), "This can take a minute while Windows runs a security scan.");
        assert_eq!(fill("short", 58), "short");
        assert_eq!(fill("one two  three\nfour", 9), "one two\nthree\nfour");
        // A word longer than the line is cut where the line ends
        assert_eq!(fill("abcdefghijkl mno", 5), "abcde\nfghij\nkl\nmno");
        assert_eq!(fill("Windows-Sicherheitsprüfung", 12), "Windows-Sich\nerheitsprüfu\nng");
        // A hyphen between letters can end a line
        assert_eq!(fill("Das kann eine Minute dauern, während Windows-Sicherheitsprüfung läuft und alles-prüft.", 30),
                   "Das kann eine Minute dauern,\nwährend Windows-\nSicherheitsprüfung läuft und\nalles-prüft.");
        assert_eq!(fill("a - b -c d--e well-known x", 8), "a - b -c\nd--e\nwell-\nknown x");
        assert_eq!(fill("", 10), "");
    }
}
