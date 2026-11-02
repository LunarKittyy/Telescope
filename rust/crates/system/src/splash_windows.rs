//! The start-up splash's window on Windows (scripts/write_splash.py's picture and telescope.spec's text): a borderless 420x180 box in
//! the middle of the screen with the app icon, "Telescope" and a status line, drawn with GDI on a thread of its own so it stays alive
//! while the main thread loads.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use crate::splash::SplashWindow;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WIDTH: i32 = 420;
const HEIGHT: i32 = 180;
const MARGIN: i32 = 28;
const ICON: i32 = 44;
/// The status text grows upwards from this line (telescope.spec's text_pos).
const TEXT_BOTTOM: i32 = 156;

const BG: u32 = rgb(0x0f, 0x12, 0x16);
const BORDER: u32 = rgb(0x2f, 0x36, 0x41);
const TEXT: u32 = rgb(0xe7, 0xea, 0xef);
const TEXT_DIM: u32 = rgb(0xa1, 0xab, 0xb8);

/// A COLORREF, which holds blue in its high byte.
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassExW {
    size: u32,
    style: u32,
    wnd_proc: Option<WndProc>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    icon_small: isize,
}

#[repr(C)]
struct PaintStruct {
    hdc: isize,
    erase: i32,
    paint: Rect,
    restore: i32,
    update: i32,
    reserved: [u8; 32],
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pixels_per_metre: i32,
    y_pixels_per_metre: i32,
    colours_used: u32,
    colours_important: u32,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(ex_style: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: isize, menu: isize, instance: isize, param: *mut c_void) -> isize;
    fn ShowWindow(hwnd: isize, command: i32) -> i32;
    fn UpdateWindow(hwnd: isize) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn InvalidateRect(hwnd: isize, rect: *const Rect, erase: i32) -> i32;
    fn BeginPaint(hwnd: isize, paint: *mut PaintStruct) -> isize;
    fn EndPaint(hwnd: isize, paint: *const PaintStruct) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn LoadCursorW(instance: isize, name: *const u16) -> isize;
    fn FillRect(hdc: isize, rect: *const Rect, brush: isize) -> i32;
    fn FrameRect(hdc: isize, rect: *const Rect, brush: isize) -> i32;
    fn DrawTextW(hdc: isize, text: *const u16, len: i32, rect: *mut Rect, format: u32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(colour: u32) -> isize;
    fn DeleteObject(object: isize) -> i32;
    fn CreateFontW(height: i32, width: i32, escapement: i32, orientation: i32, weight: i32, italic: u32, underline: u32, strike: u32, charset: u32, out_precision: u32,
                   clip_precision: u32, quality: u32, pitch_and_family: u32, face: *const u16) -> isize;
    fn SelectObject(hdc: isize, object: isize) -> isize;
    fn SetTextColor(hdc: isize, colour: u32) -> u32;
    fn SetBkMode(hdc: isize, mode: i32) -> i32;
    fn SetDIBitsToDevice(hdc: isize, x: i32, y: i32, w: u32, h: u32, x_src: i32, y_src: i32, start_scan: u32, lines: u32, bits: *const c_void, info: *const BitmapInfoHeader, usage: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetCurrentProcess() -> isize;
    fn GetProcessTimes(process: isize, created: *mut u64, exited: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    fn GetSystemTimeAsFileTime(time: *mut u64);
}

const WM_DESTROY: u32 = 0x0002;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const SW_SHOWNOACTIVATE: i32 = 4;
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;
const TRANSPARENT: i32 = 1;
const DT_LEFT: u32 = 0x0000;
const DT_VCENTER: u32 = 0x0004;
const DT_SINGLELINE: u32 = 0x0020;
const DT_NOPREFIX: u32 = 0x0800;
const DT_CALCRECT: u32 = 0x0400;
const DEFAULT_CHARSET: u32 = 1;
const CLEARTYPE_QUALITY: u32 = 5;
const FW_SEMIBOLD: i32 = 600;
const FW_NORMAL: i32 = 400;
const DIB_RGB_COLORS: u32 = 0;
/// MAKEINTRESOURCE(IDC_ARROW)
const IDC_ARROW: usize = 32512;

/// What the window paints: the status text, and the icon as top-down BGRA over the window's own colour.
struct State {
    text: String,
    icon: Vec<u8>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The app icon blended onto the background, in the order GDI wants (blue, green, red, unused).
fn icon_pixels() -> Vec<u8> {
    let bg = [0x0f as u32, 0x12, 0x16];
    crate::autostart::app_icon_rgba(ICON as u32).chunks(4).flat_map(|p| {
        let a = p[3] as u32;
        let mix = |channel: usize| ((p[channel] as u32 * a + bg[channel] * (255 - a) + 127) / 255) as u8;
        [mix(2), mix(1), mix(0), 0]
    }).collect()
}

unsafe fn text_out(hdc: isize, face: &[u16], height: i32, weight: i32, colour: u32, text: &str, rect: &mut Rect, format: u32) {
    let font = CreateFontW(-height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0, face.as_ptr());
    let old = SelectObject(hdc, font);
    SetTextColor(hdc, colour);
    let words = wide(text);
    DrawTextW(hdc, words.as_ptr(), (words.len() - 1) as i32, rect, format);
    SelectObject(hdc, old);
    DeleteObject(font);
}

unsafe fn paint(hwnd: isize) {
    let mut ps: PaintStruct = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let all = Rect { left: 0, top: 0, right: WIDTH, bottom: HEIGHT };
    let (bg, border) = (CreateSolidBrush(BG), CreateSolidBrush(BORDER));
    FillRect(hdc, &all, bg);
    FrameRect(hdc, &all, border);
    DeleteObject(bg);
    DeleteObject(border);
    SetBkMode(hdc, TRANSPARENT);
    let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        let info = BitmapInfoHeader { size: std::mem::size_of::<BitmapInfoHeader>() as u32, width: ICON, height: -ICON, planes: 1, bit_count: 32, compression: 0,
                                      size_image: 0, x_pixels_per_metre: 0, y_pixels_per_metre: 0, colours_used: 0, colours_important: 0 };
        SetDIBitsToDevice(hdc, MARGIN, MARGIN, ICON as u32, ICON as u32, 0, 0, 0, ICON as u32, state.icon.as_ptr() as *const c_void, &info, DIB_RGB_COLORS);
        let face = wide("Segoe UI");
        let mut title = Rect { left: MARGIN + ICON + 14, top: MARGIN, right: WIDTH - MARGIN, bottom: MARGIN + ICON };
        text_out(hdc, &face, 22, FW_SEMIBOLD, TEXT, "Telescope", &mut title, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        // The status lines sit above TEXT_BOTTOM: measure them, then draw the box that ends there
        let mut status = Rect { left: MARGIN, top: 0, right: WIDTH - MARGIN, bottom: 0 };
        text_out(hdc, &face, 13, FW_NORMAL, TEXT_DIM, &state.text, &mut status, DT_LEFT | DT_NOPREFIX | DT_CALCRECT);
        let height = status.bottom - status.top;
        let mut place = Rect { left: MARGIN, top: TEXT_BOTTOM - height, right: WIDTH - MARGIN, bottom: TEXT_BOTTOM };
        text_out(hdc, &face, 13, FW_NORMAL, TEXT_DIM, &state.text, &mut place, DT_LEFT | DT_NOPREFIX);
    }
    drop(guard);
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn window_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_CLOSE => {
            // DestroyWindow, through the default handler
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The splash window. Dropping it doesn't close it; close() does.
pub struct Splash {
    hwnd: isize,
}

impl Splash {
    /// Puts the splash up, on a thread of its own, once it exists; None when Windows wouldn't make the window.
    pub fn open() -> Option<Arc<Splash>> {
        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(State { text: "Starting Telescope…".into(), icon: icon_pixels() });
        let (tx, rx) = std::sync::mpsc::channel::<isize>();
        std::thread::Builder::new().name("splash".into()).spawn(move || {
            // SAFETY: plain Win32 calls with NUL-terminated strings that outlive them; the window belongs to this thread, which runs its messages
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let class = wide("TelescopeSplash");
                let spec = WndClassExW { size: std::mem::size_of::<WndClassExW>() as u32, style: 0, wnd_proc: Some(window_proc), cls_extra: 0, wnd_extra: 0, instance, icon: 0,
                                         cursor: LoadCursorW(0, IDC_ARROW as *const u16), background: 0, menu_name: std::ptr::null(), class_name: class.as_ptr(), icon_small: 0 };
                RegisterClassExW(&spec);
                let (x, y) = ((GetSystemMetrics(SM_CXSCREEN) - WIDTH) / 2, (GetSystemMetrics(SM_CYSCREEN) - HEIGHT) / 2);
                let title = wide("Telescope");
                let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), title.as_ptr(), WS_POPUP, x, y, WIDTH, HEIGHT, 0, 0, instance, std::ptr::null_mut());
                let _ = tx.send(hwnd);
                if hwnd == 0 {
                    return;
                }
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                UpdateWindow(hwnd);
                let mut msg: Msg = std::mem::zeroed();
                while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }).ok()?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(hwnd) if hwnd != 0 => Some(Arc::new(Splash { hwnd })),
            _ => None,
        }
    }
}

impl SplashWindow for Splash {
    fn update_text(&self, text: &str) {
        if let Some(state) = STATE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            state.text = text.to_string();
        }
        // SAFETY: a stale handle only makes the call fail
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
    }

    fn close(&self) {
        // SAFETY: as above; the window closes itself on its own thread
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
    }
}

/// How long ago Windows started this process, which is when the launcher showed the splash; zero when it won't say.
pub fn since_launch() -> Duration {
    let (mut created, mut exited, mut kernel, mut user, mut now) = (0u64, 0u64, 0u64, 0u64, 0u64);
    // SAFETY: each pointer is to a live u64 for the call; a FILETIME is two u32s in the same order
    let ok = unsafe {
        let ok = GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user);
        GetSystemTimeAsFileTime(&mut now);
        ok
    };
    if ok == 0 || now < created {
        return Duration::ZERO;
    }
    Duration::from_nanos((now - created) * 100)
}
