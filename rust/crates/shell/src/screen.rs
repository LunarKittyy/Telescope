//! Keeping a window that opens inside the part of the screen apps may use (the taskbar is outside it). Sizes in physical pixels.

use slint::winit_030::{winit, WinitWindowAccessor};
use slint::ComponentHandle;
use slint_interpreter::ComponentInstance;
use std::rc::Rc;
use std::time::Duration;

/// The part of a screen a window may take up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Where a window should be and how big (its inside), for whichever of the two has to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fit {
    pub position: Option<(i32, i32)>,
    pub size: Option<(u32, u32)>,
}

/// A window at `position` (where its frame starts, if the system says) with `inner` inside a frame adding `frame`, fitted into
/// `area`: cut down to it (never under `least`, the smallest inside it may have) and moved back in where its edges stick out.
/// None when it fits as it is.
pub(crate) fn fit_in(position: Option<(i32, i32)>, inner: (u32, u32), frame: (u32, u32), area: Area, least: (u32, u32)) -> Option<Fit> {
    let room = |side: u32, space: u32, frame: u32, least: u32| side.min(space.saturating_sub(frame)).max(least);
    let size = (room(inner.0, area.width, frame.0, least.0), room(inner.1, area.height, frame.1, least.1));
    let outer = (size.0 + frame.0, size.1 + frame.1);
    // Left and top edges win where it's still too big: the title bar and the menu stay reachable
    let back = |at: i32, len: u32, from: i32, space: u32| at.min(from.saturating_add_unsigned(space).saturating_sub_unsigned(len)).max(from);
    let position = position.and_then(|(x, y)| {
        let moved = (back(x, outer.0, area.x, area.width), back(y, outer.1, area.y, area.height));
        (moved != (x, y)).then_some(moved)
    });
    let size = (size != inner).then_some(size);
    (position.is_some() || size.is_some()).then_some(Fit { position, size })
}

/// Where a window of outer size `child` starts to sit in the middle of a parent whose frame starts at `parent` and measures `parent_size`.
pub(crate) fn centred_over(parent: (i32, i32), parent_size: (u32, u32), child: (u32, u32)) -> (i32, i32) {
    let middle = |at: i32, outer: u32, inner: u32| at.saturating_add((outer as i64 - inner as i64).div_euclid(2) as i32);
    (middle(parent.0, parent_size.0, child.0), middle(parent.1, parent_size.1, child.1))
}

/// Puts the window in the middle of its parent once the system has made it, then keeps it inside the screen. Where the system
/// won't say where the parent is (Wayland), the compositor's own placement stays.
pub(crate) fn centre_over(ui: &ComponentInstance, parent: &ComponentInstance, least: (u32, u32)) {
    let parent = parent.as_weak();
    when_made(ui, move |w| {
        let (inner, outer) = (w.inner_size(), w.outer_size());
        if inner.width == 0 || inner.height == 0 {
            return None;
        }
        let parent = parent.upgrade()?;
        let found = parent.window().with_winit_window(|p| p.outer_position().ok().map(|at| ((at.x, at.y), p.outer_size())));
        Some(found.flatten().map(|(at, size)| centred_over(at, (size.width, size.height), (outer.width, outer.height))))
    }, move |ui, at| {
        if let Some((x, y)) = at {
            ui.window().set_position(slint::PhysicalPosition::new(x, y));
        }
        keep_inside(ui, least);
    });
}

/// The work area of the screen the window is on, as a window the system made.
pub(crate) fn work_area(w: &winit::window::Window) -> Option<Area> {
    #[cfg(windows)]
    if let Some(area) = windows_work_area(w) {
        return Some(area);
    }
    let monitor = w.current_monitor().or_else(|| w.primary_monitor())?;
    let (at, size) = (monitor.position(), monitor.size());
    Some(Area { x: at.x, y: at.y, width: size.width, height: size.height })
}

#[cfg(windows)]
fn windows_work_area(w: &winit::window::Window) -> Option<Area> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    let RawWindowHandle::Win32(handle) = w.window_handle().ok()?.as_raw() else { return None };
    let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: hwnd is this process's window, and info is the size it says it is
    let got = unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) };
    let r = info.rcWork;
    (got.as_bool() && r.right > r.left && r.bottom > r.top)
        .then(|| Area { x: r.left, y: r.top, width: (r.right - r.left) as u32, height: (r.bottom - r.top) as u32 })
}

/// How this window fits its screen right now: None until the system has made the window, which happens on a turn of the event
/// loop after it was shown (and again with a window that has no size yet), Some(None) when it fits.
pub(crate) fn fit_of(w: &winit::window::Window, least: (u32, u32)) -> Option<Option<Fit>> {
    let (inner, outer) = (w.inner_size(), w.outer_size());
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let frame = (outer.width.saturating_sub(inner.width), outer.height.saturating_sub(inner.height));
    let area = work_area(w)?;
    let position = w.outer_position().ok().map(|p| (p.x, p.y));
    Some(fit_in(position, (inner.width, inner.height), frame, area, least))
}

/// Runs `compute` on the system's window as soon as there is one and it says Some, then `apply` with that. A window shown a
/// moment ago has no winit window until a turn of the event loop later, and a one-off look right after show() found nothing.
pub(crate) fn when_made<T: 'static>(ui: &ComponentInstance, compute: impl Fn(&winit::window::Window) -> Option<T> + 'static,
                                    apply: impl Fn(&ComponentInstance, T) + 'static) {
    attempt(ui.as_weak(), Rc::new(compute), Rc::new(apply), 200);
}

type Compute<T> = Rc<dyn Fn(&winit::window::Window) -> Option<T>>;
type Apply<T> = Rc<dyn Fn(&ComponentInstance, T)>;

fn attempt<T: 'static>(weak: slint::Weak<ComponentInstance>, compute: Compute<T>, apply: Apply<T>, left: u32) {
    let Some(ui) = weak.upgrade() else { return };
    // Applied after the call: setting a size from inside it would borrow the window twice
    if let Some(found) = ui.window().with_winit_window(|w| compute(w)).flatten() {
        apply(&ui, found);
    } else if left > 0 && ui.window().is_visible() {
        slint::Timer::single_shot(Duration::from_millis(10), move || attempt(weak, compute, apply, left - 1));
    }
}

/// Fits the window into its screen's work area once the system has made it.
pub(crate) fn keep_inside(ui: &ComponentInstance, least: (u32, u32)) {
    when_made(ui, move |w| fit_of(w, least), |ui, fit| {
        let Some(fit) = fit else { return };
        let window = ui.window();
        if let Some((width, height)) = fit.size {
            window.set_size(slint::PhysicalSize::new(width, height));
        }
        if let Some((x, y)) = fit.position {
            window.set_position(slint::PhysicalPosition::new(x, y));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = Area { x: 0, y: 0, width: 1280, height: 752 };
    const FRAME: (u32, u32) = (16, 39);

    #[test]
    fn a_window_bigger_than_the_screen_is_cut_down_and_moved_in() {
        // What the VM showed: 1380x900 inside, opened at 104,104 on a 1280x800 screen with a taskbar of 48
        let fit = fit_in(Some((104, 104)), (1380, 900), FRAME, SCREEN, (560, 520)).unwrap();
        assert_eq!(fit, Fit { position: Some((0, 0)), size: Some((1264, 713)) });
    }

    #[test]
    fn a_dialog_sits_in_the_middle_of_its_parent() {
        assert_eq!(centred_over((100, 50), (1000, 700), (800, 600)), (200, 100));
        assert_eq!(centred_over((-1920, 40), (1000, 700), (1002, 700)), (-1921, 40));
        assert_eq!(centred_over((0, 0), (500, 400), (900, 700)), (-200, -150));
    }

    #[test]
    fn a_window_that_fits_stays_as_it_is() {
        assert_eq!(fit_in(Some((100, 50)), (900, 600), FRAME, SCREEN, (560, 520)), None);
    }

    #[test]
    fn a_window_that_fits_but_sticks_out_is_moved_not_resized() {
        let fit = fit_in(Some((600, 300)), (900, 600), FRAME, SCREEN, (560, 520)).unwrap();
        assert_eq!(fit, Fit { position: Some((1280 - 916, 752 - 639)), size: None });
    }

    #[test]
    fn the_taskbar_is_left_alone_wherever_the_work_area_starts() {
        let top_bar = Area { x: 1920, y: 40, width: 1280, height: 720 };
        let fit = fit_in(Some((1900, 0)), (1380, 900), FRAME, top_bar, (560, 520)).unwrap();
        assert_eq!(fit, Fit { position: Some((1920, 40)), size: Some((1264, 681)) });
    }

    #[test]
    fn a_window_whose_position_is_unknown_is_only_cut_down() {
        let fit = fit_in(None, (1380, 900), FRAME, SCREEN, (560, 520)).unwrap();
        assert_eq!(fit, Fit { position: None, size: Some((1264, 713)) });
    }

    #[test]
    fn a_small_screen_never_takes_it_under_its_minimum() {
        let small = Area { x: 0, y: 0, width: 500, height: 400 };
        let fit = fit_in(Some((0, 0)), (1380, 900), FRAME, small, (560, 520)).unwrap();
        assert_eq!(fit.size, Some((560, 520)));
        assert_eq!(fit.position, None, "still at the corner, which keeps its title bar");
    }
}
