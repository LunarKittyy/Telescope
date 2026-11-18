//! Keeping a window that opens inside the part of the screen apps may use (the taskbar is outside it). Sizes in physical pixels.

use slint::winit_030::{winit, WinitWindowAccessor};
use slint::ComponentHandle;
use slint_interpreter::ComponentInstance;
use std::cell::Cell;
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

/// fit_in for a window whose least size is written in logical pixels, at the scale the window has now.
pub(crate) fn fit_scaled(position: Option<(i32, i32)>, inner: (u32, u32), frame: (u32, u32), area: Area, least: (f32, f32), scale: f64) -> Option<Fit> {
    let least = ((least.0 as f64 * scale).round() as u32, (least.1 as f64 * scale).round() as u32);
    fit_in(position, inner, frame, area, least)
}

/// What a window may not be cut below, and what it does with the room its screen gives it (the Settings window's own minimum follows it).
#[derive(Clone)]
pub(crate) struct Limits {
    /// The smallest inside, in logical pixels
    pub least: (f32, f32),
    relax: Option<Rc<dyn Fn(&ComponentInstance, (f32, f32))>>,
}

impl Limits {
    pub(crate) fn new(least: (f32, f32)) -> Self {
        Limits { least, relax: None }
    }

    /// The main window's: the minimum its layout needs (TelescopeWindow's min-width and min-height).
    pub(crate) fn main() -> Self {
        Self::new((560.0, 520.0))
    }

    /// `then` is told the work area in logical pixels before the window is cut down to it, so a window whose own minimum depends on the
    /// screen can lower it first.
    pub(crate) fn relaxed_by(mut self, then: impl Fn(&ComponentInstance, (f32, f32)) + 'static) -> Self {
        self.relax = Some(Rc::new(then));
        self
    }

    fn relax(&self, ui: &ComponentInstance, area: Area, scale: f64) {
        if let Some(relax) = &self.relax {
            relax(ui, (area.width as f32 / scale as f32, area.height as f32 / scale as f32));
        }
    }
}

/// Where a window of outer size `child` starts to sit in the middle of a parent whose frame starts at `parent` and measures `parent_size`.
pub(crate) fn centred_over(parent: (i32, i32), parent_size: (u32, u32), child: (u32, u32)) -> (i32, i32) {
    let middle = |at: i32, outer: u32, inner: u32| at.saturating_add((outer as i64 - inner as i64).div_euclid(2) as i32);
    (middle(parent.0, parent_size.0, child.0), middle(parent.1, parent_size.1, child.1))
}

/// Puts the window in the middle of its parent once the system has made it, then keeps it inside the screen. Where the system
/// won't say where the parent is (Wayland), the compositor's own placement stays.
pub(crate) fn centre_over(ui: &ComponentInstance, parent: &ComponentInstance, limits: &Limits) {
    let parent = parent.as_weak();
    let limits = limits.clone();
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
        keep_inside(ui, &limits);
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

/// How this window fits its screen right now, with the work area it was measured against: None until the system has made the
/// window, which happens on a turn of the event loop after it was shown (and again with a window that has no size yet),
/// Some(None) for the fit when it fits.
pub(crate) fn fit_of(w: &winit::window::Window, least: (f32, f32)) -> Option<(Area, Option<Fit>)> {
    let (inner, outer) = (w.inner_size(), w.outer_size());
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let frame = (outer.width.saturating_sub(inner.width), outer.height.saturating_sub(inner.height));
    let area = work_area(w)?;
    let position = w.outer_position().ok().map(|p| (p.x, p.y));
    Some((area, fit_scaled(position, (inner.width, inner.height), frame, area, least, w.scale_factor())))
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
pub(crate) fn keep_inside(ui: &ComponentInstance, limits: &Limits) {
    let found = limits.clone();
    let limits = limits.clone();
    when_made(ui, move |w| fit_of(w, found.least).map(|(area, fit)| (area, w.scale_factor(), fit)), move |ui, (area, scale, fit)| {
        limits.relax(ui, area, scale);
        apply(ui, fit, false);
    });
}

fn apply(ui: &ComponentInstance, fit: Option<Fit>, size_only: bool) {
    let Some(fit) = fit else { return };
    let window = ui.window();
    if let Some((width, height)) = fit.size {
        window.set_size(slint::PhysicalSize::new(width, height));
    }
    if let Some((x, y)) = fit.position.filter(|_| !size_only) {
        window.set_position(slint::PhysicalPosition::new(x, y));
    }
}

/// Why a window is looked at again.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Cause {
    Nothing,
    /// It moved: only a move onto another screen matters, and then it is cut down without being moved, so a drag isn't fought
    Moved,
    /// The scale changed (another screen's, or the display setting): cut down and moved back in
    Scale,
}

/// How long a burst of moves is left to settle before the window is looked at.
const SETTLE: Duration = Duration::from_millis(60);

/// Keeps a window inside its screen's work area for as long as it lives: what it hangs off the window's winit events (see
/// keys::watch_scan_codes_and). A scale change, or a move onto a screen with another work area, fits it again, the same way as at
/// its first showing. It is only ever cut down, never made bigger than it is.
pub(crate) fn follow(ui: &ComponentInstance, limits: Limits) -> impl FnMut(&winit::event::WindowEvent) + 'static {
    use winit::event::WindowEvent;
    let weak = ui.as_weak();
    let pending = Rc::new(Cell::new(Cause::Nothing));
    let last: Rc<Cell<Option<(Area, u64)>>> = Rc::default();
    move |event| {
        let cause = match event {
            WindowEvent::ScaleFactorChanged { .. } => Cause::Scale,
            WindowEvent::Moved(_) => Cause::Moved,
            _ => return,
        };
        // A look is already on its way: it learns of the stronger cause
        if pending.replace(pending.get().max(cause)) != Cause::Nothing {
            return;
        }
        let (weak, pending, last, limits) = (weak.clone(), pending.clone(), last.clone(), limits.clone());
        slint::Timer::single_shot(SETTLE, move || {
            let cause = pending.replace(Cause::Nothing);
            if let Some(ui) = weak.upgrade() {
                refit(&ui, &limits, cause, &last);
            }
        });
    }
}

fn refit(ui: &ComponentInstance, limits: &Limits, cause: Cause, last: &Cell<Option<(Area, u64)>>) {
    let found = ui.window().with_winit_window(|w| fit_of(w, limits.least).map(|(area, fit)| (area, w.scale_factor(), fit))).flatten();
    let Some((area, scale, fit)) = found else { return };
    let key = Some((area, scale.to_bits()));
    if last.replace(key) == key && cause != Cause::Scale {
        return;
    }
    limits.relax(ui, area, scale);
    apply(ui, fit, cause != Cause::Scale);
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

    // A 1920x1080 screen, as the Windows laptop has it, with a taskbar of 48 physical pixels on one edge. Settings (940x680 logical) and the
    // main window (1380x900 logical) at the scale the person has set, their frame (invisible borders and title bar) as Windows gives it
    const SETTINGS: (f32, f32) = (940.0, 680.0);
    const MAIN: (f32, f32) = (1380.0, 900.0);
    const MAIN_LEAST: (f32, f32) = (560.0, 520.0);
    const SETTINGS_LEAST: (f32, f32) = (320.0, 240.0);
    const WINDOWS_FRAME: (u32, u32) = (22, 56);

    fn physical((w, h): (f32, f32), scale: f64) -> (u32, u32) {
        ((w as f64 * scale).round() as u32, (h as f64 * scale).round() as u32)
    }

    fn bar(edge: &str) -> Area {
        match edge {
            "bottom" => Area { x: 0, y: 0, width: 1920, height: 1032 },
            "top" => Area { x: 0, y: 48, width: 1920, height: 1032 },
            "left" => Area { x: 48, y: 0, width: 1872, height: 1080 },
            "right" => Area { x: 0, y: 0, width: 1872, height: 1080 },
            _ => unreachable!(),
        }
    }

    #[test]
    fn settings_at_150_percent_stops_above_a_taskbar_at_the_bottom() {
        // What the laptop showed: 1432x1076 outside on a work area of 1032, its bottom under the taskbar
        let fit = fit_scaled(Some((244, 0)), physical(SETTINGS, 1.5), WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, 1.5).unwrap();
        assert_eq!(fit, Fit { position: None, size: Some((1410, 976)) });
    }

    #[test]
    fn a_taskbar_at_the_top_moves_the_window_below_it() {
        let fit = fit_scaled(Some((244, 0)), physical(SETTINGS, 1.5), WINDOWS_FRAME, bar("top"), SETTINGS_LEAST, 1.5).unwrap();
        assert_eq!(fit, Fit { position: Some((244, 48)), size: Some((1410, 976)) });
    }

    #[test]
    fn a_taskbar_at_the_left_or_right_cuts_the_width_and_keeps_clear_of_it() {
        let main = physical(MAIN, 1.5);
        assert_eq!(main, (2070, 1350));
        let left = fit_scaled(Some((0, 0)), main, (16, 39), bar("left"), MAIN_LEAST, 1.5).unwrap();
        assert_eq!(left, Fit { position: Some((48, 0)), size: Some((1856, 1041)) });
        let right = fit_scaled(Some((60, 0)), main, (16, 39), bar("right"), MAIN_LEAST, 1.5).unwrap();
        assert_eq!(right, Fit { position: Some((0, 0)), size: Some((1856, 1041)) });
    }

    #[test]
    fn the_main_window_at_125_percent_is_cut_to_the_work_area() {
        let main = physical(MAIN, 1.25);
        assert_eq!(main, (1725, 1125));
        let fit = fit_scaled(Some((0, 0)), main, (16, 39), bar("bottom"), MAIN_LEAST, 1.25).unwrap();
        assert_eq!(fit, Fit { position: None, size: Some((1725, 993)) });
    }

    #[test]
    fn what_fits_at_one_scale_is_cut_when_the_scale_grows() {
        // 1100x700 logical is fine at 100% and 125%, and 150% makes it 1650x1050 inside on a work area of 1032 high
        let at = |scale: f64| fit_scaled(Some((100, 40)), physical((1100.0, 700.0), scale), WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, scale);
        assert_eq!(at(1.0), None);
        assert_eq!(at(1.25), None);
        assert_eq!(at(1.5), Some(Fit { position: Some((100, 0)), size: Some((1650, 976)) }));
    }

    #[test]
    fn a_window_is_never_made_bigger_than_it_is() {
        let small = physical((700.0, 500.0), 1.5);
        assert_eq!(fit_scaled(Some((10, 10)), small, WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, 1.5), None);
        assert_eq!(fit_scaled(None, small, WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, 1.5), None);
    }

    #[test]
    fn a_work_area_smaller_than_the_minimum_leaves_the_minimum() {
        // 1280x720 logical monitor at 150% less a taskbar is still bigger; a tiny one is not: the minimum (in logical pixels, at the scale) wins
        let tiny = Area { x: 0, y: 0, width: 700, height: 500 };
        let fit = fit_scaled(Some((0, 0)), physical(MAIN, 1.5), (16, 39), tiny, MAIN_LEAST, 1.5).unwrap();
        assert_eq!(fit.size, Some((840, 780)), "560x520 at 150%");
        assert_eq!(fit.position, None, "its corner and title bar stay on the screen");
        let fit = fit_scaled(Some((0, 0)), physical(MAIN, 1.25), (16, 39), tiny, MAIN_LEAST, 1.25).unwrap();
        assert_eq!(fit.size, Some((700, 650)), "560x520 at 125%");
    }

    #[test]
    fn the_least_size_is_logical_so_it_grows_with_the_scale() {
        let area = Area { x: 0, y: 0, width: 1000, height: 700 };
        let at = |scale: f64| fit_scaled(None, physical(MAIN, scale), (16, 39), area, MAIN_LEAST, scale).unwrap().size.unwrap();
        assert_eq!(at(1.0), (984, 661));
        assert_eq!(at(1.25), (984, 661));
        assert_eq!(at(1.5), (984, 780), "the height is cut no further than 520 logical");
    }

    #[test]
    fn a_stronger_cause_wins_when_several_arrive_together() {
        assert!(Cause::Scale > Cause::Moved && Cause::Moved > Cause::Nothing);
    }
}
