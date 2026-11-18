//! The frame the system draws around a window: on Windows the title bar is the top bar's colour in the look that is on (Windows 11; Windows
//! 10 keeps its own) and shows the app's icon. On KDE Plasma it's the same colours (kde.rs). GNOME and the other desktops keep the system's
//! own frame.

use slint_interpreter::ComponentInstance;

/// Dresses the frame of a window that was just shown, once the system has made it, and again when the look changes (the look changes).
pub(crate) fn dress(ui: &ComponentInstance) {
    #[cfg(windows)]
    {
        let colours = (colour(ui, "chrome"), colour(ui, "ink"));
        crate::screen::when_made(ui, hwnd_of, move |_, hwnd| windows_frame(hwnd, colours));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    crate::kde::dress(ui);
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    let _ = ui;
}

/// A Theme colour as 0xBBGGRR, the order Windows wants colours in.
#[cfg(windows)]
fn colour(ui: &ComponentInstance, name: &str) -> Option<u32> {
    match ui.get_global_property("Theme", name) {
        Ok(slint_interpreter::Value::Brush(b)) => {
            let c = b.color();
            Some(c.red() as u32 | (c.green() as u32) << 8 | (c.blue() as u32) << 16)
        }
        _ => None,
    }
}

#[cfg(windows)]
fn hwnd_of(w: &slint::winit_030::winit::window::Window) -> Option<isize> {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(windows)]
fn windows_frame(hwnd: isize, (caption, text): (Option<u32>, Option<u32>)) {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE};
    let hwnd = HWND(hwnd as *mut c_void);
    let set = |attribute, value: u32| {
        // SAFETY: hwnd is this process's window and value outlives the call at the size passed; Windows 10 refuses the colours, which is fine
        let _ = unsafe { DwmSetWindowAttribute(hwnd, attribute, &value as *const u32 as *const c_void, 4) };
    };
    // A dark look gets a dark title bar (it darkens the buttons as well), Light a light one; the caption takes the top bar's colour and the
    // text the bar's ink, both Windows 11 only
    let dark = caption.is_none_or(|c| (c & 0xff) + (c >> 8 & 0xff) + (c >> 16 & 0xff) < 3 * 128);
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(dark));
    if let Some(c) = caption {
        set(DWMWA_CAPTION_COLOR, c);
    }
    if let Some(c) = text {
        set(DWMWA_TEXT_COLOR, c);
    }
    set_icons(hwnd);
}

/// The program's icon, resource 1 (build.rs), at the sizes the title bar and the alt-tab list use. Loaded shared, so the system owns the
/// handles (one per size, the same each time, never destroyed here) and a window that has them already is left as it is.
#[cfg(windows)]
fn set_icons(hwnd: windows::Win32::Foundation::HWND) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, SM_CXICON, SM_CXSMICON,
        SM_CYICON, SM_CYSMICON, WM_GETICON, WM_SETICON,
    };
    // SAFETY: the module is this program's and resource 1 is an icon; a shared handle belongs to the system and must not be destroyed
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
        for (kind, w, h) in [(ICON_SMALL, SM_CXSMICON, SM_CYSMICON), (ICON_BIG, SM_CXICON, SM_CYICON)] {
            let size = (GetSystemMetrics(w), GetSystemMetrics(h));
            if let Ok(icon) = LoadImageW(Some(module.into()), PCWSTR(std::ptr::without_provenance(1)), IMAGE_ICON, size.0, size.1, LR_DEFAULTCOLOR | LR_SHARED) {
                let has = SendMessageW(hwnd, WM_GETICON, Some(WPARAM(kind as usize)), Some(LPARAM(0)));
                if has.0 != icon.0 as isize {
                    SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
                }
            }
        }
    }
}
