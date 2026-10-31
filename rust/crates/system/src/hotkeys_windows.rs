//! Windows global shortcuts (platform/hotkeys_windows.py): RegisterHotKey on a thread of its own, whose message loop
//! gets the WM_HOTKEYs.
//!
//! Windows never says when the key comes back up, so for "while held" the thread watches the key (GetAsyncKeyState)
//! once it went down. No hook, no admin rights: a key another app already registered just fails, and the page says
//! so. Windows hands that key to the other app even while Telescope is in front, so it doesn't work inside either.

use crate::hotkeys::{Hotkey, HotkeyStatus, Signals};
use crate::keys;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use telescope_core::i18n::tr;

pub const MOD_NOREPEAT: u32 = 0x4000;
const VK_CONTROL: u8 = 0x11;
#[cfg_attr(not(windows), allow(dead_code))]
const RELEASE_POLL_MS: u32 = 30;

/// The few user32 calls the backend makes, so tests can stand in for Windows.
pub trait User32 {
    fn register(&mut self, number: i32, flags: u32, vk: u32) -> bool;
    fn unregister(&mut self, number: i32);
    fn key_down(&self, vk: u32) -> bool;
    /// VkKeyScanW: low byte the key, 0x100 when the layout needs Shift for it, -1 for none.
    fn vk_scan(&self, ch: char) -> i16;
    fn send_key(&mut self, vk: u8, up: bool);
}

pub type Statuses = Arc<Mutex<HashMap<String, HotkeyStatus>>>;

/// What the hotkey thread keeps: which RegisterHotKey number is which binding, and which keys are down.
pub struct Registrar<U: User32> {
    pub user32: U,
    /// RegisterHotKey's id: (hotkey id, virtual-key code, modifier flags).
    by_number: HashMap<i32, (String, u32, u32)>,
    hotkeys: Vec<Hotkey>,
    /// Hotkey id: virtual-key code, while its key is down.
    held: HashMap<String, u32>,
    statuses: Statuses,
    signals: Signals,
}

impl<U: User32> Registrar<U> {
    pub fn new(user32: U, statuses: Statuses, signals: Signals) -> Registrar<U> {
        Registrar { user32, by_number: HashMap::new(), hotkeys: Vec::new(), held: HashMap::new(), statuses, signals }
    }

    pub fn apply(&mut self, hotkeys: &[Hotkey]) {
        self.hotkeys = hotkeys.to_vec();
        self.unregister_all();
        let mut statuses = HashMap::new();
        for (i, hk) in hotkeys.iter().enumerate() {
            let number = i as i32 + 1;
            let user32 = &self.user32;
            let Some((flags, vk)) = keys::to_windows(&hk.keys, Some(&|ch| user32.vk_scan(ch))) else {
                statuses.insert(hk.id.clone(), HotkeyStatus::new(false, tr("Windows can't use this key everywhere")));
                continue;
            };
            if self.user32.register(number, flags | MOD_NOREPEAT, vk) {
                self.by_number.insert(number, (hk.id.clone(), vk, flags));
                statuses.insert(hk.id.clone(), HotkeyStatus::new(true, tr("Everywhere")));
            } else {
                let status = HotkeyStatus { working: false, text: tr("Another app already uses this key"), reaches_app: false };
                statuses.insert(hk.id.clone(), status);
            }
        }
        *self.statuses.lock().unwrap() = statuses;
        self.signals.changed();
    }

    pub fn suspend(&mut self, on: bool) {
        if on {
            self.unregister_all();
        } else {
            let hotkeys = self.hotkeys.clone();
            self.apply(&hotkeys);
        }
    }

    pub fn unregister_all(&mut self) {
        for number in self.by_number.keys().copied().collect::<Vec<_>>() {
            self.user32.unregister(number);
        }
        self.by_number.clear();
        for id in self.held.drain().map(|(id, _)| id).collect::<Vec<_>>() {
            self.signals.released(&id);
        }
    }

    pub fn on_hotkey(&mut self, number: i32) {
        let Some((id, vk, flags)) = self.by_number.get(&number).cloned() else { return };
        if self.held.contains_key(&id) {
            // MOD_NOREPEAT should already stop these
            return;
        }
        self.held.insert(id.clone(), vk);
        self.signals.pressed(&id);
        if flags & keys::MOD_ALT != 0 && flags & keys::MOD_WIN == 0 {
            self.mask_alt();
        }
    }

    /// Windows keeps the key itself, so the app in front sees Alt go down and up alone and shows its menu key tips.
    fn mask_alt(&mut self) {
        // A held Ctrl stays held
        let order = if self.user32.key_down(VK_CONTROL as u32) { [true, false] } else { [false, true] };
        for up in order {
            self.user32.send_key(VK_CONTROL, up);
        }
    }

    /// One WM_HOTKEY for the thread's loop: from here it watches for the release.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    fn hotkey_came(&mut self, number: i32, watching: &mut bool) {
        self.on_hotkey(number);
        *watching = true;
    }

    /// The end of a turn of the thread's loop: how long to wait before looking at the keys again, None until something comes.
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    fn next_wait(&mut self, watching: &mut bool) -> Option<u32> {
        if *watching {
            *watching = self.check_released();
        }
        watching.then_some(RELEASE_POLL_MS)
    }

    /// Lets go of the keys that came back up; whether any are still down.
    pub fn check_released(&mut self) -> bool {
        let up: Vec<String> = self.held.iter().filter(|(_, vk)| !self.user32.key_down(**vk)).map(|(id, _)| id.clone()).collect();
        for id in up {
            self.held.remove(&id);
            self.signals.released(&id);
        }
        !self.held.is_empty()
    }
}

#[cfg(windows)]
pub use real::WindowsHotkeys;

#[cfg(windows)]
mod real {
    use super::*;
    use crate::hotkeys::HotkeyBackend;
    use std::sync::mpsc::{channel, Sender};

    const WM_HOTKEY: u32 = 0x0312;
    const WM_APP: u32 = 0x8000;
    const PM_REMOVE: u32 = 0x1;
    const QS_ALLINPUT: u32 = 0x04FF;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const KEYEVENTF_KEYUP: u32 = 0x2;

    #[repr(C)]
    struct Msg {
        hwnd: *mut core::ffi::c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
        private: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterHotKey(hwnd: *mut core::ffi::c_void, id: i32, modifiers: u32, vk: u32) -> i32;
        fn UnregisterHotKey(hwnd: *mut core::ffi::c_void, id: i32) -> i32;
        fn GetAsyncKeyState(vk: i32) -> i16;
        fn VkKeyScanW(ch: u16) -> i16;
        fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
        fn PeekMessageW(msg: *mut Msg, hwnd: *mut core::ffi::c_void, min: u32, max: u32, remove: u32) -> i32;
        fn PostThreadMessageW(thread: u32, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn MsgWaitForMultipleObjects(count: u32, handles: *const *mut core::ffi::c_void, all: i32, ms: u32, mask: u32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    struct Real;

    impl User32 for Real {
        fn register(&mut self, number: i32, flags: u32, vk: u32) -> bool {
            // SAFETY: plain values; a null window posts WM_HOTKEY to this thread's queue
            unsafe { RegisterHotKey(std::ptr::null_mut(), number, flags, vk) != 0 }
        }
        fn unregister(&mut self, number: i32) {
            // SAFETY: plain values
            unsafe { UnregisterHotKey(std::ptr::null_mut(), number) };
        }
        fn key_down(&self, vk: u32) -> bool {
            // SAFETY: plain value
            (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
        }
        fn vk_scan(&self, ch: char) -> i16 {
            let mut units = [0u16; 2];
            if ch.encode_utf16(&mut units).len() != 1 {
                return -1;
            }
            // SAFETY: plain value
            unsafe { VkKeyScanW(units[0]) }
        }
        fn send_key(&mut self, vk: u8, up: bool) {
            // SAFETY: plain values
            unsafe { keybd_event(vk, 0, if up { KEYEVENTF_KEYUP } else { 0 }, 0) };
        }
    }

    enum Cmd {
        Apply(Vec<Hotkey>),
        Suspend(bool),
        Stop,
    }

    pub struct WindowsHotkeys {
        cmds: Sender<Cmd>,
        thread_id: u32,
        statuses: Statuses,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl WindowsHotkeys {
        pub fn new(signals: Signals) -> WindowsHotkeys {
            let statuses: Statuses = Arc::default();
            let (tx, rx) = channel();
            let (id_tx, id_rx) = channel();
            let shared = statuses.clone();
            let thread = std::thread::Builder::new().name("windows-hotkeys".into()).spawn(move || {
                let mut msg: Msg = unsafe { std::mem::zeroed() };
                // SAFETY: makes this thread's message queue, so posts to it from now on aren't lost
                unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, 0) };
                let _ = id_tx.send(unsafe { GetCurrentThreadId() });
                let mut reg = Registrar::new(Real, shared, signals);
                let mut watching = false;
                loop {
                    // SAFETY: msg is a live Msg for each call
                    while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                        if msg.message == WM_HOTKEY {
                            reg.hotkey_came(msg.wparam as i32, &mut watching);
                        }
                    }
                    for cmd in rx.try_iter() {
                        match cmd {
                            Cmd::Apply(h) => reg.apply(&h),
                            Cmd::Suspend(on) => reg.suspend(on),
                            Cmd::Stop => {
                                reg.unregister_all();
                                return;
                            }
                        }
                    }
                    let wait = reg.next_wait(&mut watching).unwrap_or(INFINITE);
                    // SAFETY: no handles, just this thread's queue
                    unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, wait, QS_ALLINPUT) };
                }
            }).ok();
            let thread_id = id_rx.recv().unwrap_or(0);
            WindowsHotkeys { cmds: tx, thread_id, statuses, thread }
        }

        fn send(&self, cmd: Cmd) {
            if self.cmds.send(cmd).is_ok() {
                // SAFETY: plain values; wakes the thread's wait
                unsafe { PostThreadMessageW(self.thread_id, WM_APP, 0, 0) };
            }
        }
    }

    impl HotkeyBackend for WindowsHotkeys {
        fn apply(&mut self, hotkeys: &[Hotkey]) {
            self.send(Cmd::Apply(hotkeys.to_vec()));
        }
        fn status(&self, id: &str) -> Option<HotkeyStatus> {
            self.statuses.lock().unwrap().get(id).cloned()
        }
        fn suspend(&mut self, on: bool) {
            self.send(Cmd::Suspend(on));
        }
        fn note(&self) -> String {
            tr("Everywhere shortcuts are taken from Windows while Telescope runs. A key another app already took doesn't reach Telescope at all, so pick another.")
        }
        fn name(&self) -> &'static str {
            "WindowsHotkeys"
        }
        fn stop(&mut self) {
            self.send(Cmd::Stop);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for WindowsHotkeys {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{MOD_ALT, MOD_CONTROL};
    use serde_json::Value;
    use std::collections::HashSet;
    use telescope_core::host::Poster;

    #[derive(Default)]
    struct Fake {
        taken: HashSet<(u32, u32)>,
        registered: HashMap<i32, (u32, u32)>,
        down: HashSet<u32>,
        sent: Vec<(u8, &'static str)>,
    }

    impl User32 for Fake {
        fn register(&mut self, number: i32, flags: u32, vk: u32) -> bool {
            if self.taken.contains(&(flags & !MOD_NOREPEAT, vk)) {
                return false;
            }
            assert!(flags & MOD_NOREPEAT != 0);
            self.registered.insert(number, (flags, vk));
            true
        }
        fn unregister(&mut self, number: i32) {
            self.registered.remove(&number);
        }
        fn key_down(&self, vk: u32) -> bool {
            self.down.contains(&vk)
        }
        fn vk_scan(&self, _ch: char) -> i16 {
            -1
        }
        fn send_key(&mut self, vk: u8, up: bool) {
            self.sent.push((vk, if up { "up" } else { "down" }));
        }
    }

    struct Collect(Arc<Mutex<Vec<Value>>>);

    impl Poster for Collect {
        fn post(&self, _token: &str, data: Value) {
            self.0.lock().unwrap().push(data);
        }
    }

    fn registrar(fake: Fake) -> (Registrar<Fake>, Arc<Mutex<Vec<Value>>>) {
        let posted = Arc::new(Mutex::new(Vec::new()));
        (Registrar::new(fake, Statuses::default(), Signals(Arc::new(Collect(posted.clone())))), posted)
    }

    fn hk(id: &str, keys: &str) -> Hotkey {
        Hotkey { id: id.into(), keys: keys.into(), description: String::new() }
    }

    fn presses(posted: &Mutex<Vec<Value>>) -> Vec<String> {
        posted.lock().unwrap().iter().filter(|d| d["kind"] != "changed")
            .map(|d| format!("{} {}", d["kind"].as_str().unwrap(), d["id"].as_str().unwrap())).collect()
    }

    #[test]
    fn registers_and_reports() {
        let (mut reg, posted) = registrar(Fake { taken: HashSet::from([(MOD_CONTROL, 'T' as u32)]), ..Default::default() });
        reg.apply(&[hk("a", "Ctrl+Alt+M"), hk("b", "Ctrl+T"), hk("c", "Ctrl+/")]);
        assert_eq!(reg.user32.registered, HashMap::from([(1, (MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'M' as u32))]));
        let st = |id: &str| reg.statuses.lock().unwrap()[id].clone();
        assert!(st("a").working);
        assert_eq!(st("b").text, "Another app already uses this key");
        // Windows gives it to the other app
        assert!(st("a").reaches_app && !st("b").reaches_app);
        assert!(st("c").reaches_app && !st("c").working);
        assert!(posted.lock().unwrap().iter().any(|d| d["kind"] == "changed"));
        reg.apply(&[]);
        assert!(reg.user32.registered.is_empty());
    }

    #[test]
    fn presses_and_watches_for_the_release() {
        let (mut reg, posted) = registrar(Fake::default());
        reg.apply(&[hk("a", "Ctrl+Space")]);
        let mut watching = false;
        assert_eq!(reg.next_wait(&mut watching), None, "nothing to watch yet: it sleeps until a message comes");
        reg.user32.down.insert(0x20);
        reg.hotkey_came(1, &mut watching);
        // Still down: not pressed twice
        reg.hotkey_came(1, &mut watching);
        // Not ours
        reg.hotkey_came(99, &mut watching);
        assert_eq!(reg.next_wait(&mut watching), Some(RELEASE_POLL_MS), "the loop comes back by itself");
        assert_eq!(presses(&posted), ["pressed a"]);
        assert_eq!(reg.next_wait(&mut watching), Some(RELEASE_POLL_MS));
        assert_eq!(presses(&posted), ["pressed a"]);
        reg.user32.down.clear();
        assert_eq!(reg.next_wait(&mut watching), None);
        assert_eq!(presses(&posted), ["pressed a", "released a"]);
    }

    #[test]
    fn masks_alt_so_the_app_in_front_shows_no_menu_keys() {
        let (mut reg, _) = registrar(Fake::default());
        reg.apply(&[hk("a", "Alt+M"), hk("b", "Ctrl+Alt+K"), hk("c", "Meta+Alt+J"), hk("d", "Ctrl+Shift+L")]);
        reg.on_hotkey(1);
        assert_eq!(reg.user32.sent, [(0x11, "down"), (0x11, "up")]);
        reg.user32.sent.clear();
        // Ctrl is part of the key and still held: it stays held
        reg.user32.down.insert(0x11);
        reg.on_hotkey(2);
        assert_eq!(reg.user32.sent, [(0x11, "up"), (0x11, "down")]);
        reg.user32.sent.clear();
        // Windows already keeps a Win key from opening Start
        reg.on_hotkey(3);
        reg.on_hotkey(4);
        assert!(reg.user32.sent.is_empty());
    }

    #[test]
    fn lets_go_while_suspended() {
        let (mut reg, _) = registrar(Fake::default());
        reg.apply(&[hk("a", "Ctrl+F1")]);
        reg.suspend(true);
        assert!(reg.user32.registered.is_empty());
        reg.suspend(false);
        assert_eq!(reg.user32.registered.values().copied().collect::<Vec<_>>(), [(MOD_CONTROL | MOD_NOREPEAT, 0x70)]);
    }
}
