//! The Windows tray (Qt's QSystemTrayIcon in the Python app): Shell_NotifyIconW from a hidden window on a thread of its
//! own, whose message loop hears the icon's clicks. A left click says "" (show or hide the window, as Python's Trigger),
//! a right click opens the menu. Notifications show as the icon's balloon, as Python's tray.showMessage.
//!
//! What the menu shows and what the icon's messages mean are plain functions, so tests run them anywhere.

use telescope_core::system::TrayItem;

pub const MF_GRAYED: u32 = 0x1;
pub const MF_CHECKED: u32 = 0x8;
pub const MF_SEPARATOR: u32 = 0x800;
const WM_CONTEXTMENU: u32 = 0x007B;
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;
/// szTip's length, the end's 0 included.
pub const TIP_UNITS: usize = 128;

/// One menu line: its command (TrackPopupMenu's answer), text and MF_* flags.
#[derive(Debug, PartialEq)]
pub struct MenuLine {
    pub command: u32,
    pub text: String,
    pub flags: u32,
}

/// The visible items as menu lines; an item with no title is a separator. Commands are the item's place plus one,
/// since TrackPopupMenu answers 0 for nothing picked.
pub fn menu_lines(items: &[TrayItem]) -> Vec<MenuLine> {
    items.iter().enumerate().filter(|(_, i)| i.visible).map(|(n, i)| {
        let command = n as u32 + 1;
        if i.title.is_empty() {
            return MenuLine { command, text: String::new(), flags: MF_SEPARATOR };
        }
        let mut flags = 0;
        if !i.enabled {
            flags |= MF_GRAYED;
        }
        if i.checkable && i.checked {
            flags |= MF_CHECKED;
        }
        // A lone & underlines the next letter in a Win32 menu
        MenuLine { command, text: i.title.replace('&', "&&"), flags }
    }).collect()
}

/// The item id a picked command stands for.
pub fn picked(items: &[TrayItem], command: u32) -> Option<String> {
    let item = items.get((command as usize).checked_sub(1)?)?;
    (item.visible && item.enabled && !item.title.is_empty()).then(|| item.id.clone())
}

#[derive(Debug, PartialEq)]
pub enum IconEvent {
    /// Clicked, or picked from the keyboard: show or hide the window.
    Activate,
    Menu,
    Other,
}

/// What the icon's callback message says (NOTIFYICON_VERSION_4: the event in lParam's low word).
pub fn icon_event(lparam: isize) -> IconEvent {
    match (lparam as u32) & 0xFFFF {
        NIN_SELECT | NIN_KEYSELECT => IconEvent::Activate,
        WM_CONTEXTMENU => IconEvent::Menu,
        _ => IconEvent::Other,
    }
}

/// text in a fixed UTF-16 field of n units, cut short to leave its closing 0 (szTip, szInfo, szInfoTitle).
pub fn wide_field<const N: usize>(text: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (slot, unit) in out.iter_mut().take(N - 1).zip(text.encode_utf16()) {
        *slot = unit;
    }
    // Not half a surrogate pair at the cut
    if (0xD800..0xDC00).contains(&out[N.saturating_sub(2)]) {
        out[N - 2] = 0;
    }
    out
}

#[cfg(windows)]
pub use real::{balloon, WindowsTray};

#[cfg(windows)]
mod real {
    use super::*;
    use crate::icon::TrayShows;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};
    use telescope_core::system::Tray;

    type Hwnd = *mut c_void;

    const WM_DESTROY: u32 = 0x0002;
    const WM_CLOSE: u32 = 0x0010;
    const WM_NULL: u32 = 0x0000;
    const WM_APP: u32 = 0x8000;
    const WM_ICON: u32 = WM_APP + 1;
    const WM_TIP: u32 = WM_APP + 2;
    const WM_BALLOON: u32 = WM_APP + 3;
    const WM_SHOWS: u32 = WM_APP + 4;
    const SM_CXSMICON: i32 = 49;
    const NIM_ADD: u32 = 0;
    const NIM_MODIFY: u32 = 1;
    const NIM_DELETE: u32 = 2;
    const NIM_SETVERSION: u32 = 4;
    const NIF_MESSAGE: u32 = 0x1;
    const NIF_ICON: u32 = 0x2;
    const NIF_TIP: u32 = 0x4;
    const NIF_INFO: u32 = 0x10;
    const NIF_SHOWTIP: u32 = 0x80;
    const NIIF_INFO: u32 = 0x1;
    const NIIF_WARNING: u32 = 0x2;
    const NOTIFYICON_VERSION_4: u32 = 4;
    const TPM_RIGHTBUTTON: u32 = 0x2;
    const TPM_NONOTIFY: u32 = 0x80;
    const TPM_RETURNCMD: u32 = 0x100;
    const IDI_APPLICATION: usize = 32512;

    #[repr(C)]
    struct NotifyIconData {
        size: u32,
        hwnd: Hwnd,
        id: u32,
        flags: u32,
        callback_message: u32,
        icon: *mut c_void,
        tip: [u16; TIP_UNITS],
        state: u32,
        state_mask: u32,
        info: [u16; 256],
        version: u32,
        info_title: [u16; 64],
        info_flags: u32,
        guid: [u8; 16],
        balloon_icon: *mut c_void,
    }

    #[repr(C)]
    struct WndClassEx {
        size: u32,
        style: u32,
        proc_: unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu_name: *const u16,
        class_name: *const u16,
        icon_small: *mut c_void,
    }

    #[repr(C)]
    struct Msg {
        hwnd: Hwnd,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
        private: u32,
    }

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(class: *const WndClassEx) -> u16;
        fn CreateWindowExW(ex_style: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32,
                           parent: Hwnd, menu: *mut c_void, instance: *mut c_void, param: *mut c_void) -> Hwnd;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn DestroyWindow(hwnd: Hwnd) -> i32;
        fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
        fn TranslateMessage(msg: *const Msg) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
        fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn PostQuitMessage(code: i32);
        fn RegisterWindowMessageW(name: *const u16) -> u32;
        fn CreatePopupMenu() -> *mut c_void;
        fn AppendMenuW(menu: *mut c_void, flags: u32, id: usize, text: *const u16) -> i32;
        fn TrackPopupMenu(menu: *mut c_void, flags: u32, x: i32, y: i32, reserved: i32, hwnd: Hwnd, rect: *const c_void) -> i32;
        fn DestroyMenu(menu: *mut c_void) -> i32;
        fn SetForegroundWindow(hwnd: Hwnd) -> i32;
        fn GetCursorPos(point: *mut Point) -> i32;
        fn LoadIconW(instance: *mut c_void, name: *const u16) -> *mut c_void;
        fn CreateIcon(instance: *mut c_void, width: i32, height: i32, planes: u8, bits: u8, and_mask: *const u8, xor_mask: *const u8) -> *mut c_void;
        fn DestroyIcon(icon: *mut c_void) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconData) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    #[derive(Default)]
    struct State {
        items: Vec<TrayItem>,
        tooltip: String,
        clicks: VecDeque<String>,
        balloons: VecDeque<(String, String, bool)>,
        shows: TrayShows,
    }

    type Shared = Arc<Mutex<State>>;

    /// What the window procedure needs, on the tray's own thread.
    struct Here {
        shared: Shared,
        instance: *mut c_void,
        icon: Cell<*mut c_void>,
        // Whether icon is one this made, for destroying; LoadIconW's isn't
        drawn: Cell<bool>,
        taskbar_created: u32,
    }

    thread_local! {
        static HERE: RefCell<Option<Here>> = const { RefCell::new(None) };
    }

    /// Where notifications go once a tray is up: its window, as a number so it can cross threads.
    static BALLOONS: Mutex<Option<(isize, Shared)>> = Mutex::new(None);

    fn data(hwnd: Hwnd, here: &Here) -> NotifyIconData {
        // SAFETY: a C struct of plain numbers, pointers and arrays, for which all zeroes is valid
        let mut d: NotifyIconData = unsafe { std::mem::zeroed() };
        d.size = std::mem::size_of::<NotifyIconData>() as u32;
        d.hwnd = hwnd;
        d.id = 1;
        d.icon = here.icon.get();
        d
    }

    /// The tray's picture at the size Windows draws it, from the same pixels as Linux; null if Windows won't take it.
    fn icon_for(instance: *mut c_void, shows: TrayShows) -> *mut c_void {
        // SAFETY: a plain query
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
        let bgra: Vec<u8> = crate::icon::tray_rgba(size as u32, shows).as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
        // Zeros: with 32-bit colour the alpha says what shows
        let mask = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
        // SAFETY: both masks are the size the width and height ask for and live through the call, which copies them
        unsafe { CreateIcon(instance, size, size, 1, 32, mask.as_ptr(), bgra.as_ptr()) }
    }

    fn add_icon(hwnd: Hwnd, here: &Here) {
        let mut d = data(hwnd, here);
        d.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.callback_message = WM_ICON;
        d.tip = wide_field(&here.shared.lock().unwrap().tooltip);
        // SAFETY: d is a filled NOTIFYICONDATAW whose size field matches
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &mut d);
            d.version = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &mut d);
        }
    }

    fn show_menu(hwnd: Hwnd, here: &Here) {
        let items = here.shared.lock().unwrap().items.clone();
        let lines = menu_lines(&items);
        if lines.is_empty() {
            return;
        }
        // SAFETY: the menu is made, shown and destroyed here; texts outlive the AppendMenuW calls
        unsafe {
            let menu = CreatePopupMenu();
            for line in &lines {
                let text = wide(&line.text);
                AppendMenuW(menu, line.flags, line.command as usize, text.as_ptr());
            }
            let mut at = Point { x: 0, y: 0 };
            GetCursorPos(&mut at);
            // Without this the menu stays open when clicking elsewhere
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, at.x, at.y, 0, hwnd, std::ptr::null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            if let Some(id) = picked(&items, command.max(0) as u32) {
                here.shared.lock().unwrap().clicks.push_back(id);
            }
        }
    }

    unsafe extern "system" fn window_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
        let handled = HERE.with(|h| {
            let h = h.borrow();
            let here = h.as_ref()?;
            match msg {
                WM_ICON => match icon_event(lparam) {
                    IconEvent::Activate => here.shared.lock().unwrap().clicks.push_back(String::new()),
                    IconEvent::Menu => show_menu(hwnd, here),
                    IconEvent::Other => {}
                },
                WM_SHOWS => {
                    let shows = here.shared.lock().unwrap().shows;
                    let icon = icon_for(here.instance, shows);
                    if !icon.is_null() {
                        let mut d = data(hwnd, here);
                        d.flags = NIF_ICON;
                        d.icon = icon;
                        // SAFETY: a filled NOTIFYICONDATAW, and the old icon isn't used once the new one is in
                        unsafe {
                            Shell_NotifyIconW(NIM_MODIFY, &mut d);
                            if here.drawn.replace(true) {
                                DestroyIcon(here.icon.replace(icon));
                            } else {
                                here.icon.set(icon);
                            }
                        }
                    }
                }
                WM_TIP => {
                    let mut d = data(hwnd, here);
                    d.flags = NIF_TIP | NIF_SHOWTIP;
                    d.tip = wide_field(&here.shared.lock().unwrap().tooltip);
                    // SAFETY: a filled NOTIFYICONDATAW
                    unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut d) };
                }
                WM_BALLOON => {
                    let queued: Vec<_> = here.shared.lock().unwrap().balloons.drain(..).collect();
                    for (title, body, urgent) in queued {
                        let mut d = data(hwnd, here);
                        d.flags = NIF_INFO;
                        d.info_title = wide_field(&title);
                        d.info = wide_field(&body);
                        d.info_flags = if urgent { NIIF_WARNING } else { NIIF_INFO };
                        // SAFETY: a filled NOTIFYICONDATAW
                        unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut d) };
                    }
                }
                WM_CLOSE => {
                    let mut d = data(hwnd, here);
                    // SAFETY: a filled NOTIFYICONDATAW; this window is ours
                    unsafe {
                        Shell_NotifyIconW(NIM_DELETE, &mut d);
                        DestroyWindow(hwnd);
                    }
                }
                WM_DESTROY => unsafe { PostQuitMessage(0) },
                // Explorer started again (it crashed or restarted): the icon has to be added anew
                m if m == here.taskbar_created && m != 0 => add_icon(hwnd, here),
                _ => return None,
            }
            Some(0)
        });
        // SAFETY: the default handling for everything not handled above
        handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
    }

    pub struct WindowsTray {
        shared: Shared,
        hwnd: isize,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl WindowsTray {
        /// The icon in the notification area, or None when the window for it couldn't be made.
        pub fn new() -> Option<WindowsTray> {
            let shared = Shared::default();
            let theirs = shared.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let thread = std::thread::Builder::new().name("windows-tray".into()).spawn(move || {
                let class = wide("TelescopeTray");
                // SAFETY: the class and window are this thread's; every pointer passed lives through its call
                let hwnd = unsafe {
                    let instance = GetModuleHandleW(std::ptr::null());
                    let wc = WndClassEx {
                        size: std::mem::size_of::<WndClassEx>() as u32, style: 0, proc_: window_proc, cls_extra: 0, wnd_extra: 0,
                        instance, icon: std::ptr::null_mut(), cursor: std::ptr::null_mut(), background: std::ptr::null_mut(),
                        menu_name: std::ptr::null(), class_name: class.as_ptr(), icon_small: std::ptr::null_mut(),
                    };
                    RegisterClassExW(&wc);
                    // A hidden top-level window rather than a message-only one, which never hears TaskbarCreated
                    let title = wide("Telescope");
                    let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(),
                                               std::ptr::null_mut(), instance, std::ptr::null_mut());
                    if !hwnd.is_null() {
                        // Drawn as on Linux; Windows' generic one if that fails
                        let mut icon = icon_for(instance, TrayShows::default());
                        let drawn = !icon.is_null();
                        if !drawn {
                            icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION as *const u16);
                        }
                        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
                        let here = Here { shared: theirs, instance, icon: Cell::new(icon), drawn: Cell::new(drawn), taskbar_created };
                        add_icon(hwnd, &here);
                        HERE.with(|h| *h.borrow_mut() = Some(here));
                    }
                    hwnd
                };
                let _ = tx.send(hwnd as isize);
                if hwnd.is_null() {
                    return;
                }
                // SAFETY: msg is a live Msg for each call
                let mut msg: Msg = unsafe { std::mem::zeroed() };
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                HERE.with(|h| *h.borrow_mut() = None);
            }).ok()?;
            let hwnd = rx.recv().unwrap_or(0);
            if hwnd == 0 {
                let _ = thread.join();
                log::warn!("Couldn't make the tray icon's window");
                return None;
            }
            *BALLOONS.lock().unwrap() = Some((hwnd, shared.clone()));
            Some(WindowsTray { shared, hwnd, thread: Some(thread) })
        }

        fn post(&self, msg: u32) {
            // SAFETY: plain values; the window belongs to the tray thread, which handles it there
            unsafe { PostMessageW(self.hwnd as Hwnd, msg, 0, 0) };
        }
    }

    impl Tray for WindowsTray {
        fn set_items(&mut self, items: &[TrayItem]) {
            // The menu is built from these each time it opens
            self.shared.lock().unwrap().items = items.to_vec();
        }

        fn set_tooltip(&mut self, text: &str) {
            {
                let mut st = self.shared.lock().unwrap();
                if st.tooltip == text {
                    return;
                }
                st.tooltip = text.into();
            }
            self.post(WM_TIP);
        }

        fn set_state(&mut self, camera: bool, mic: Option<bool>) {
            let shows = TrayShows::from_stream(camera, mic);
            {
                let mut st = self.shared.lock().unwrap();
                if st.shows == shows {
                    return;
                }
                st.shows = shows;
            }
            self.post(WM_SHOWS);
        }

        fn poll_click(&mut self) -> Option<String> {
            self.shared.lock().unwrap().clicks.pop_front()
        }
    }

    impl Drop for WindowsTray {
        fn drop(&mut self) {
            *BALLOONS.lock().unwrap() = None;
            self.post(WM_CLOSE);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Shows a notification as the tray icon's balloon; false when there's no tray to show it from.
    pub fn balloon(title: &str, body: &str, urgent: bool) -> bool {
        let Some((hwnd, shared)) = BALLOONS.lock().unwrap().clone() else { return false };
        shared.lock().unwrap().balloons.push_back((title.into(), body.into(), urgent));
        // SAFETY: plain values; the tray thread handles it
        unsafe { PostMessageW(hwnd as Hwnd, WM_BALLOON, 0, 0) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str) -> TrayItem {
        TrayItem { id: id.into(), title: title.into(), checkable: false, checked: false, visible: true, enabled: true }
    }

    #[test]
    fn the_menu_shows_visible_items_with_their_state() {
        let mut keep = item("keep", "Keep in tray");
        keep.checkable = true;
        keep.checked = true;
        let mut off = item("off", "Off for now");
        off.enabled = false;
        let mut hidden = item("hidden", "Hidden");
        hidden.visible = false;
        let items = [item("show", "Show Telescope"), keep, item("", ""), off, hidden, item("quit", "Quit & close")];
        let lines = menu_lines(&items);
        assert_eq!(lines.iter().map(|l| (l.command, l.text.as_str(), l.flags)).collect::<Vec<_>>(), [
            (1, "Show Telescope", 0), (2, "Keep in tray", MF_CHECKED), (3, "", MF_SEPARATOR), (4, "Off for now", MF_GRAYED),
            (6, "Quit && close", 0),
        ]);
        assert_eq!(picked(&items, 6).as_deref(), Some("quit"));
        assert_eq!(picked(&items, 0), None, "nothing picked");
        assert_eq!(picked(&items, 3), None, "a separator");
        assert_eq!(picked(&items, 4), None, "greyed out");
        assert_eq!(picked(&items, 5), None, "not shown");
        assert_eq!(picked(&items, 99), None);
    }

    #[test]
    fn a_left_click_activates_and_a_right_click_opens_the_menu() {
        assert_eq!(icon_event(0x0400), IconEvent::Activate);
        assert_eq!(icon_event(0x0401), IconEvent::Activate);
        // The icon's id rides in the high word
        assert_eq!(icon_event(0x0001_007B), IconEvent::Menu);
        assert_eq!(icon_event(0x0200), IconEvent::Other, "mouse moves");
    }

    #[test]
    fn long_texts_are_cut_to_fit_with_their_closing_zero() {
        let tip: [u16; TIP_UNITS] = wide_field(&"x".repeat(300));
        assert_eq!(tip[TIP_UNITS - 2], u16::from(b'x'));
        assert_eq!(tip[TIP_UNITS - 1], 0);
        let short: [u16; 8] = wide_field("Hi");
        assert_eq!(short[..3], [u16::from(b'H'), u16::from(b'i'), 0]);
        // An emoji is two units; cut through the middle it goes altogether
        let cut: [u16; 4] = wide_field("ab😀");
        assert_eq!(cut, [u16::from(b'a'), u16::from(b'b'), 0, 0]);
    }
}
