//! Keys as text (telescope/shortcuts.py's key half): Qt's portable form ("Ctrl+Alt+M"), which saved bindings use,
//! read and written without Qt, plus what the portal and Windows call the same keys.

/// Qt's portable names for keys that aren't one character (F-keys aside), so a binding saved by the Qt app loads.
const NAMED: &[&str] = &[
    "Esc", "Tab", "Backtab", "Backspace", "Return", "Enter", "Ins", "Del", "Pause", "Print", "SysReq", "Clear",
    "Home", "End", "Left", "Up", "Right", "Down", "PgUp", "PgDown", "CapsLock", "NumLock", "ScrollLock", "Menu",
    "Help", "Code input", "Multiple Candidate", "Previous Candidate", "Kanji", "Muhenkan", "Henkan", "Romaji",
    "Hiragana", "Katakana", "Hiragana Katakana", "Zenkaku", "Hankaku", "Zenkaku Hankaku", "Touroku", "Massyo",
    "Kana Lock", "Kana Shift", "Eisu Shift", "Eisu toggle", "Hangul", "Hangul Start", "Hangul End", "Hangul Hanja",
    "Hangul Jamo", "Hangul Romaja", "Hangul Jeonja", "Hangul Banja", "Hangul PreHanja", "Hangul PostHanja",
    "Hangul Special", "Back", "Forward", "Stop", "Refresh", "Volume Down", "Volume Mute", "Volume Up", "Bass Boost",
    "Bass Up", "Bass Down", "Treble Up", "Treble Down", "Media Play", "Media Stop", "Media Previous", "Media Next",
    "Media Record", "Home Page", "Favorites", "Search", "Standby", "Open URL", "Launch Mail", "Launch Media",
    "Launch (0)", "Launch (1)", "Launch (2)", "Launch (3)", "Launch (4)", "Launch (5)", "Launch (6)", "Launch (7)",
    "Launch (8)", "Launch (9)", "Launch (A)", "Launch (B)", "Launch (C)", "Launch (D)", "Launch (E)", "Launch (F)",
    "Select", "Yes", "No", "Context1", "Context2", "Context3", "Context4", "Call", "Hangup", "Flip", "Execute",
    "Printer", "Play", "Sleep", "Zoom", "Cancel", "Monitor Brightness Up", "Monitor Brightness Down",
    "Keyboard Light On/Off", "Keyboard Brightness Up", "Keyboard Brightness Down", "Power Off", "Wake Up", "Eject",
    "Screensaver", "WWW", "Memo", "LightBulb", "Shop", "History", "Add Favorite", "Hot Links", "Adjust Brightness",
    "Finance", "Community", "Media Rewind", "Back Forward", "Application Left", "Application Right", "Book", "CD",
    "Calculator", "To-do list", "Clear Grab", "Close", "Copy", "Cut", "Display", "DOS", "Documents", "Spreadsheet",
    "Browser", "Game", "Go", "iTouch", "Logoff", "Market", "Meeting", "Keyboard Menu", "Menu PB", "My Sites", "News",
    "Home Office", "Option", "Paste", "Phone", "Calendar", "Reply", "Reload", "Rotate Windows", "Rotation PB",
    "Rotation KB", "Save", "Send", "Spellchecker", "Split Screen", "Support", "Task Panel", "Terminal", "Tools",
    "Travel", "Video", "Word Processor", "XFer", "Zoom In", "Zoom Out", "Away", "Messenger", "WebCam", "Mail Forward",
    "Pictures", "Music", "Battery", "Bluetooth", "Wireless", "Ultra Wide Band", "Media Fast Forward", "Audio Repeat",
    "Audio Random Play", "Subtitle", "Audio Cycle Track", "Time", "Hibernate", "View", "Top Menu", "Power Down",
    "Suspend", "Adjust contrast", "Media Pause", "Toggle Media Play/Pause", "Launch (G)", "Launch (H)",
    "Toggle Call/Hangup", "Voice Dial", "Last Number Redial", "Camera Shutter", "Camera Focus", "Touchpad Toggle",
    "Touchpad On", "Touchpad Off", "Microphone Mute", "Red", "Green", "Yellow", "Blue", "Channel Up", "Channel Down",
    "Guide", "Info", "Settings", "Exit", "Microphone Volume Up", "Microphone Volume Down", "New", "Open", "Find",
    "Undo", "Redo", "Keyboard",
];

/// Other spellings QKeySequence.fromString takes.
const ALIASES: &[(&str, &str)] =
    &[("escape", "Esc"), ("delete", "Del"), ("insert", "Ins"), ("page up", "PgUp"), ("page down", "PgDown")];

/// Keys no text box types with (shortcuts.py's _NEVER_TYPES).
const NEVER_TYPES: &[&str] = &[
    "Pause", "Print", "ScrollLock", "Volume Mute", "Volume Down", "Volume Up", "Media Play", "Toggle Media Play/Pause",
    "Media Stop", "Media Next", "Media Previous", "Media Pause", "Microphone Mute",
];

/// One key with its modifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    /// Its portable name: "M", "F5", "PgUp", "+".
    pub key: String,
}

impl Combo {
    /// Portable text, modifiers in Qt's order: "Meta+Ctrl+Alt+Shift+A".
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (on, name) in [(self.meta, "Meta+"), (self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+")] {
            if on {
                out.push_str(name);
            }
        }
        out + &self.key
    }

    /// The modifiers in split_keys' order: Ctrl, Alt, Shift, Meta.
    pub fn modifiers(&self) -> Vec<&'static str> {
        [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift"), (self.meta, "Meta")]
            .into_iter().filter(|(on, _)| *on).map(|(_, n)| n).collect()
    }

    fn char_key(&self) -> Option<char> {
        let mut chars = self.key.chars();
        let c = chars.next()?;
        chars.next().is_none().then_some(c)
    }

    fn f_number(&self) -> Option<u32> {
        self.key.strip_prefix('F').and_then(|n| n.parse().ok()).filter(|n| (1..=35).contains(n))
    }
}

fn key_name(text: &str) -> Option<String> {
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        // Qt names one character by its upper case, and a space as Space
        return Some(if c == ' ' { "Space".into() } else { c.to_uppercase().collect() });
    }
    let lower = text.to_lowercase();
    if lower == "space" {
        return Some("Space".into());
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=35).contains(&n).then(|| format!("F{n}"));
    }
    if let Some((_, name)) = ALIASES.iter().find(|(a, _)| *a == lower) {
        return Some((*name).into());
    }
    NAMED.iter().find(|n| n.to_lowercase() == lower).map(|n| (*n).into())
}

/// Reads portable text: one key with its modifiers, or None (two chords, a bare modifier, nonsense).
pub fn parse(text: &str) -> Option<Combo> {
    // Qt takes a trailing ", " after one chord, and nothing after it
    let text = text.strip_suffix(", ").unwrap_or(text);
    let mut combo = Combo { ctrl: false, alt: false, shift: false, meta: false, key: String::new() };
    let mut rest = text;
    loop {
        let lower = rest.to_lowercase();
        let flag = if lower.starts_with("ctrl+") {
            &mut combo.ctrl
        } else if lower.starts_with("alt+") {
            &mut combo.alt
        } else if lower.starts_with("shift+") {
            &mut combo.shift
        } else if lower.starts_with("meta+") {
            &mut combo.meta
        } else {
            break;
        };
        *flag = true;
        rest = &rest[rest.find('+').unwrap() + 1..];
    }
    if rest.contains(", ") && rest.chars().count() > 1 {
        return None;
    }
    combo.key = key_name(rest)?;
    Some(combo)
}

/// One key with its modifiers in portable text ("Ctrl+Alt+M"); "" for anything else.
pub fn normalize_keys(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    parse(text).map(|c| c.text()).unwrap_or_default()
}

/// A Slint key event as portable text, or "" for a bare modifier or a key with no name.
pub fn keys_from_slint(text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> String {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return String::new() };
    let mut shift = shift;
    let key = match c {
        '\u{8}' => "Backspace".to_string(),
        '\t' => "Tab".into(),
        '\u{19}' => {
            // Shift+Tab comes as its own key, as in Qt
            shift = true;
            "Tab".into()
        }
        '\n' | '\r' => "Return".into(),
        '\u{1b}' => "Esc".into(),
        '\u{7f}' => "Del".into(),
        ' ' => "Space".into(),
        '\u{F700}' => "Up".into(),
        '\u{F701}' => "Down".into(),
        '\u{F702}' => "Left".into(),
        '\u{F703}' => "Right".into(),
        '\u{F704}'..='\u{F726}' => format!("F{}", c as u32 - 0xF704 + 1),
        '\u{F727}' => "Ins".into(),
        '\u{F729}' => "Home".into(),
        '\u{F72B}' => "End".into(),
        '\u{F72C}' => "PgUp".into(),
        '\u{F72D}' => "PgDown".into(),
        '\u{F72F}' => "ScrollLock".into(),
        '\u{F730}' => "Pause".into(),
        '\u{F731}' => "SysReq".into(),
        '\u{F734}' => "Stop".into(),
        '\u{F735}' => "Menu".into(),
        '\u{F748}' => "Back".into(),
        // The modifiers themselves, Caps Lock among them, and the rest of the private range
        c if (c as u32) < 0x20 || ('\u{F700}'..='\u{F8FF}').contains(&c) => return String::new(),
        c => c.to_uppercase().collect(),
    };
    Combo { ctrl, alt, shift, meta, key }.text()
}

/// Whether a key press from Slint is Ctrl, Alt, Shift or Meta alone (Caps Lock isn't one).
pub fn is_bare_modifier(text: &str) -> bool {
    matches!(text, "\u{10}" | "\u{11}" | "\u{12}" | "\u{13}" | "\u{15}" | "\u{16}" | "\u{17}" | "\u{18}")
}

/// What this system calls the Windows/Super key.
pub const META_NAME: &str = if cfg!(windows) { "Win" } else { "Meta" };

/// keys as this system writes them: the Windows/Super key as META_NAME.
pub fn native_keys(keys: &str) -> String {
    native_keys_for(keys, META_NAME)
}

fn native_keys_for(keys: &str, meta: &str) -> String {
    if meta == "Meta" { keys.to_string() } else { keys.replace("Meta+", &format!("{meta}+")) }
}

/// Whether keys holds Ctrl, Alt or Meta, so typing into a text box can't set it off by accident.
pub fn has_command_modifier(keys: &str) -> bool {
    parse(keys).is_some_and(|c| c.ctrl || c.alt || c.meta)
}

/// Whether a text box would use keys: a plain key that types or moves the cursor, not an F-key or media key.
pub fn edits_text(keys: &str) -> bool {
    let Some(c) = parse(keys) else { return false };
    !(c.ctrl || c.alt || c.meta || c.f_number().is_some() || NEVER_TYPES.contains(&c.key.as_str()))
}

/// Whether a text box has a command of its own on keys: copy, paste, cut, undo, redo, select all, and moving or deleting by word or to the ends.
pub fn is_text_command(keys: &str) -> bool {
    let Some(c) = parse(keys) else { return false };
    if !c.ctrl || c.alt || c.meta {
        return false;
    }
    let letter = c.char_key().is_some_and(|ch| matches!(ch.to_ascii_uppercase(), 'A' | 'C' | 'V' | 'X' | 'Y' | 'Z'));
    letter || matches!(c.key.as_str(), "Left" | "Right" | "Home" | "End" | "Backspace" | "Del" | "Ins")
}

/// Whether a focused text box keeps keys for itself: what it types or moves the cursor with, and its editing commands.
pub fn text_box_takes(keys: &str) -> bool {
    edits_text(keys) || is_text_command(keys)
}

/// keys as the shortcuts portal writes a trigger ("CTRL+ALT+m"), or "" for a key it has no name for.
pub fn to_trigger(keys: &str) -> String {
    let Some(c) = parse(keys) else { return String::new() };
    let key = match c.char_key() {
        Some(ch) if ch.is_ascii_uppercase() => ch.to_ascii_lowercase().to_string(),
        Some(ch) if ch.is_ascii_digit() => ch.to_string(),
        _ if c.f_number().is_some() => c.key.clone(),
        _ => keysym(&c.key).unwrap_or("").to_string(),
    };
    if key.is_empty() {
        return String::new();
    }
    let mods = c.modifiers().into_iter().map(|m| match m {
        "Ctrl" => "CTRL",
        "Alt" => "ALT",
        "Shift" => "SHIFT",
        _ => "LOGO",
    });
    mods.chain([key.as_str()]).collect::<Vec<_>>().join("+")
}

fn keysym(name: &str) -> Option<&'static str> {
    Some(match name {
        "Space" => "space",
        "Return" => "Return",
        "Enter" => "KP_Enter",
        "Esc" => "Escape",
        "Tab" => "Tab",
        "Backspace" => "BackSpace",
        "Ins" => "Insert",
        "Del" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "PgUp" => "Page_Up",
        "PgDown" => "Page_Down",
        "Left" => "Left",
        "Up" => "Up",
        "Right" => "Right",
        "Down" => "Down",
        "Pause" => "Pause",
        "Print" => "Print",
        "ScrollLock" => "Scroll_Lock",
        "Volume Mute" => "XF86AudioMute",
        "Volume Down" => "XF86AudioLowerVolume",
        "Volume Up" => "XF86AudioRaiseVolume",
        "Media Play" | "Toggle Media Play/Pause" => "XF86AudioPlay",
        "Media Stop" => "XF86AudioStop",
        "Media Next" => "XF86AudioNext",
        "Media Previous" => "XF86AudioPrev",
        "+" => "plus",
        "-" => "minus",
        "," => "comma",
        "." => "period",
        "/" => "slash",
        "\\" => "backslash",
        ";" => "semicolon",
        "'" => "apostrophe",
        "[" => "bracketleft",
        "]" => "bracketright",
        "`" => "grave",
        "=" => "equal",
        "*" => "asterisk",
        "#" => "numbersign",
        _ => return None,
    })
}

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// (modifier flags, virtual-key code) for keys, or None for a key Windows can't register. vk_for_char gives
/// VkKeyScanW's answer for punctuation, which depends on the keyboard layout.
pub fn to_windows(keys: &str, vk_for_char: Option<&dyn Fn(char) -> i16>) -> Option<(u32, u32)> {
    let c = parse(keys)?;
    let mut flags = 0;
    for (on, flag) in [(c.ctrl, MOD_CONTROL), (c.alt, MOD_ALT), (c.shift, MOD_SHIFT), (c.meta, MOD_WIN)] {
        if on {
            flags |= flag;
        }
    }
    if let Some(ch) = c.char_key().filter(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()) {
        return Some((flags, ch as u32));
    }
    if let Some(n) = c.f_number().filter(|n| *n <= 24) {
        return Some((flags, 0x70 + n - 1));
    }
    let vk = match c.key.as_str() {
        "Space" => 0x20,
        "Return" | "Enter" => 0x0D,
        "Esc" => 0x1B,
        "Tab" => 0x09,
        "Backspace" => 0x08,
        "Ins" => 0x2D,
        "Del" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PgUp" => 0x21,
        "PgDown" => 0x22,
        "Left" => 0x25,
        "Up" => 0x26,
        "Right" => 0x27,
        "Down" => 0x28,
        "Pause" => 0x13,
        "Print" => 0x2C,
        "ScrollLock" => 0x91,
        "Volume Mute" => 0xAD,
        "Volume Down" => 0xAE,
        "Volume Up" => 0xAF,
        "Media Next" => 0xB0,
        "Media Previous" => 0xB1,
        "Media Stop" => 0xB2,
        "Media Play" | "Toggle Media Play/Pause" => 0xB3,
        _ => 0,
    };
    if vk != 0 {
        return Some((flags, vk));
    }
    let ch = c.char_key()?;
    let scan = vk_for_char?(ch);
    if scan == -1 || scan & 0xFF == 0xFF {
        return None;
    }
    if scan & 0x100 != 0 {
        // The layout needs Shift for this character
        flags |= MOD_SHIFT;
    }
    Some((flags, (scan & 0xFF) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keys_reads_qt_portable_text() {
        for (raw, want) in [
            ("Ctrl+Alt+M", "Ctrl+Alt+M"), ("ctrl+alt+m", "Ctrl+Alt+M"), ("Meta+F5", "Meta+F5"), ("Shift+Up", "Shift+Up"),
            ("Ctrl+Alt+M, Ctrl+X", ""), ("Ctrl", ""), ("", ""), ("Nonsense+Key", ""), ("Page Up", "PgUp"),
            ("Escape", "Esc"), ("Ctrl++", "Ctrl++"), ("Ctrl+ ", "Ctrl+Space"), ("Ctrl+ä", "Ctrl+Ä"), ("F36", ""),
            ("Alt+Shift+Ctrl+Meta+a", "Meta+Ctrl+Alt+Shift+A"), ("Volume Mute", "Volume Mute"), ("Ctrl+Alt+M, ", "Ctrl+Alt+M"),
            ("Ctrl+Alt+M,", ""), ("Control+M", ""), ("A+B", ""),
        ] {
            assert_eq!(normalize_keys(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn slint_key_events_drop_bare_modifiers_and_name_shift_tab() {
        assert_eq!(keys_from_slint("m", true, false, false, false), "Ctrl+M");
        assert_eq!(keys_from_slint("\u{11}", true, false, false, false), "");
        assert_eq!(keys_from_slint("\u{19}", false, false, true, false), "Shift+Tab");
        assert_eq!(keys_from_slint("\u{F708}", false, false, false, true), "Meta+F5");
        assert_eq!(keys_from_slint("\u{F72C}", false, false, true, false), "Shift+PgUp");
        assert_eq!(keys_from_slint("ab", false, false, false, false), "");
    }

    #[test]
    fn modifiers_and_typing_keys() {
        let c = parse("Ctrl+Shift+F5").unwrap();
        assert_eq!((c.modifiers(), c.key.as_str()), (vec!["Ctrl", "Shift"], "F5"));
        assert!(has_command_modifier("Alt+M"));
        assert!(!has_command_modifier("Shift+M"));
        assert!(!has_command_modifier("F9"));
        assert!(edits_text("Z") && edits_text("Space") && edits_text("Shift+Left"));
        assert!(!edits_text("F8") && !edits_text("Volume Mute") && !edits_text("Ctrl+Z"));
        // The editing commands are the text box's too, though they hold Ctrl
        for command in ["Ctrl+A", "Ctrl+C", "Ctrl+V", "Ctrl+X", "Ctrl+Z", "Ctrl+Y", "Ctrl+Shift+Z", "Ctrl+Left", "Ctrl+Shift+Right", "Ctrl+Backspace", "Ctrl+Del", "Ctrl+Home"] {
            assert!(is_text_command(command) && text_box_takes(command), "{command}");
        }
        for other in ["Ctrl+T", "Ctrl+Alt+V", "Ctrl+Alt+Del", "Alt+Left", "Meta+C", "F8", "C", "Ctrl+F5"] {
            assert!(!is_text_command(other), "{other}");
        }
        assert!(text_box_takes("Z") && !text_box_takes("Ctrl+T"));
    }

    #[test]
    fn native_keys_names_the_windows_key_after_the_system() {
        assert_eq!(native_keys_for("Meta+Alt+W", "Meta"), "Meta+Alt+W");
        assert_eq!(native_keys_for("Meta+Alt+W", "Win"), "Win+Alt+W");
        assert_eq!(native_keys_for("Ctrl+M", "Win"), "Ctrl+M");
    }

    #[test]
    fn portal_triggers() {
        for (keys, want) in [
            ("Ctrl+Alt+M", "CTRL+ALT+m"), ("Meta+F5", "LOGO+F5"), ("Shift+Page Up", "SHIFT+Page_Up"), ("Ctrl+1", "CTRL+1"),
            ("Ctrl+=", "CTRL+equal"), ("Volume Mute", "XF86AudioMute"), ("Ctrl+§", ""),
        ] {
            assert_eq!(to_trigger(keys), want, "{keys}");
        }
    }

    #[test]
    fn windows_keys() {
        assert_eq!(to_windows("Ctrl+Alt+M", None), Some((MOD_CONTROL | MOD_ALT, 'M' as u32)));
        assert_eq!(to_windows("Meta+Shift+5", None), Some((MOD_WIN | MOD_SHIFT, '5' as u32)));
        assert_eq!(to_windows("F13", None), Some((0, 0x7C)));
        assert_eq!(to_windows("Ctrl+Up", None), Some((MOD_CONTROL, 0x26)));
        assert_eq!(to_windows("Media Play", None), Some((0, 0xB3)));
        // VkKeyScanW: low byte the key, 0x100 when the layout needs Shift for it
        assert_eq!(to_windows("Ctrl+!", Some(&|_| 0x131)), Some((MOD_CONTROL | MOD_SHIFT, 0x31)));
        assert_eq!(to_windows("Ctrl+/", Some(&|_| -1)), None);
        assert_eq!(to_windows("Ctrl+/", None), None);
    }
}
