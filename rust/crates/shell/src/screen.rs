//! Keeping a window inside the part of the screen apps may use (the taskbar is outside it). Sizes in physical pixels unless a name says logical.

use slint::winit_030::{winit, WinitWindowAccessor};
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

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

/// What was measured of a window and the screen it is on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seen {
    /// Where its frame starts, if the system says
    pub position: Option<(i32, i32)>,
    pub inner: (u32, u32),
    /// What the frame adds to the inside
    pub frame: (u32, u32),
    pub area: Area,
    /// The system said what the work area is (Windows, X11). Where it didn't (Wayland) `area` is the whole screen, which only shows what can never fit
    pub exact: bool,
    pub scale: f64,
    /// Maximized, minimized or full screen: the system places it
    pub held: bool,
    /// Its frame reaches onto another screen too
    pub straddles: bool,
}

/// Whether a window may be moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    /// Cut down only, as while it is dragged
    No,
    /// Not where it sits across two screens, as once a drag has ended
    UnlessStraddling,
    Yes,
}

/// What a window may be cut down to and how it is measured, in logical pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// The smallest inside it is made with: it is lowered to what a small work area leaves, but not below `floor`
    pub least: (f32, f32),
    pub floor: (f32, f32),
    /// What it keeps clear of the work area's edges
    pub margin: f32,
    /// The size the person is taken to want before they resize it: where None, the size it has when first measured
    pub preferred: Option<(f32, f32)>,
    /// The window has least-width and least-height properties for its minimum, which follow `least`
    pub lowers_least: bool,
    /// A cut keeps its shape (the pop-out)
    pub keeps_shape: bool,
}

impl Limits {
    pub(crate) fn new(least: (f32, f32)) -> Self {
        Limits { least, floor: least, margin: 0.0, preferred: None, lowers_least: false, keeps_shape: false }
    }

    /// The main window's: the minimum its layout needs (TelescopeWindow's least-width and least-height), where a short screen lowers the height (the columns scroll).
    pub(crate) fn main() -> Self {
        Limits { floor: (560.0, 320.0), lowers_least: true, ..Self::new((560.0, 520.0)) }
    }

    /// Settings: opens at 940x680 clear of the edges, and a small screen wins over its minimum.
    pub(crate) fn settings() -> Self {
        Limits { floor: (320.0, 240.0), margin: 48.0, preferred: Some((940.0, 680.0)), lowers_least: true, ..Self::new((720.0, 480.0)) }
    }

    /// A window that keeps its shape when it is cut down.
    pub(crate) fn shaped(least: (f32, f32)) -> Self {
        Limits { keeps_shape: true, ..Self::new(least) }
    }

    /// The minimum on a screen that leaves `room` (logical pixels) for the inside.
    pub(crate) fn least_for(&self, room: (f32, f32)) -> (f32, f32) {
        (self.least.0.min(room.0).max(self.floor.0), self.least.1.min(room.1).max(self.floor.1))
    }
}

fn physical((w, h): (f64, f64), scale: f64) -> (u32, u32) {
    ((w * scale).round() as u32, (h * scale).round() as u32)
}

/// How a window `seen` should be: the size `preferred` (logical) cut down to the room the work area leaves (never under the minimum), and moved back in
/// where its edges stick out. None when it fits as it is, and for a window the system places. Where the work area is not known it is only cut down when
/// it is bigger than the whole screen: a guess never shrinks it or grows it.
pub(crate) fn decide(seen: &Seen, preferred: (f64, f64), limits: &Limits, mv: Move) -> Option<Fit> {
    if seen.held {
        return None;
    }
    let margin = if seen.exact { (limits.margin as f64 * seen.scale).round() as u32 } else { 0 };
    let room = (seen.area.width.saturating_sub(seen.frame.0 + margin), seen.area.height.saturating_sub(seen.frame.1 + margin));
    let least = limits.least_for((room.0 as f32 / seen.scale as f32, room.1 as f32 / seen.scale as f32));
    let least = physical((least.0 as f64, least.1 as f64), seen.scale);
    let wanted = if seen.exact { physical(preferred, seen.scale) } else { seen.inner };
    let mut size = (wanted.0.min(room.0).max(least.0), wanted.1.min(room.1).max(least.1));
    if limits.keeps_shape && size != wanted && wanted.0 > 0 && wanted.1 > 0 {
        let by = (size.0 as f64 / wanted.0 as f64).min(size.1 as f64 / wanted.1 as f64);
        size = ((wanted.0 as f64 * by).round().max(1.0) as u32, (wanted.1 as f64 * by).round().max(1.0) as u32);
    }
    let outer = (size.0 + seen.frame.0, size.1 + seen.frame.1);
    let area = seen.area;
    // Left and top edges win where it's still too big: the title bar and the menu stay reachable
    let back = |at: i32, len: u32, from: i32, space: u32| at.min(from.saturating_add_unsigned(space).saturating_sub_unsigned(len)).max(from);
    let position = match (mv, seen.position) {
        (Move::No, _) | (_, None) => None,
        (Move::UnlessStraddling, _) if seen.straddles => None,
        (_, Some((x, y))) => {
            let moved = (back(x, outer.0, area.x, area.width), back(y, outer.1, area.y, area.height));
            (moved != (x, y)).then_some(moved)
        }
    };
    let size = (size != seen.inner).then_some(size);
    (position.is_some() || size.is_some()).then_some(Fit { position, size })
}

/// Whether a frame at `at` of size `outer` reaches onto any of the other screens.
pub(crate) fn reaches_onto(at: (i32, i32), outer: (u32, u32), others: &[Area]) -> bool {
    others.iter().any(|o| {
        let (right, bottom) = (at.0 as i64 + outer.0 as i64, at.1 as i64 + outer.1 as i64);
        (at.0 as i64) < o.x as i64 + o.width as i64 && right > o.x as i64 && (at.1 as i64) < o.y as i64 + o.height as i64 && bottom > o.y as i64
    })
}

/// What a window keeps between looks: the size the person wants, what was done to it on its own, and what it was last measured against.
pub(crate) struct Placement {
    limits: Limits,
    /// The size the person wants, in logical pixels: what they opened it at or last dragged it to, never what a cut left
    preferred: Cell<Option<(f64, f64)>>,
    /// The insides this set lately (logical pixels, newest last), so its own resizes aren't taken for the person's even when they arrive late or at another scale
    auto: RefCell<Vec<(f64, f64)>>,
    /// The scale the sizes seen last belong to
    scale: Cell<f64>,
    /// The work area and scale (bits) it was last measured against
    shown: Cell<Option<(Area, u64)>>,
    /// It was maximized, minimized or full screen at the last look
    held: Cell<bool>,
    /// The inside the work area leaves, as the last look found
    room: Cell<Option<(u32, u32)>>,
    pending: Cell<Cause>,
    /// It moved onto another screen and the drag has not ended yet
    drifted: Cell<bool>,
    /// Counts the moves, so a drag's end is the one nothing follows
    moves: Cell<u64>,
    /// A new work area and scale that was seen once and has to be seen again before the window is fitted to it, so a taskbar that is still sliding is not
    candidate: Cell<Option<(Area, u64)>>,
    /// A second look at `candidate` is already on its way
    watching: Cell<bool>,
    /// Until then a size the window reports is the system finishing a resize (ours, or a scale change), not the person's
    quiet_until: Cell<Option<Instant>>,
}

/// What `Placement::plan` found: how to fit it and the minimum it should have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plan {
    pub fit: Option<Fit>,
    /// The minimum inside in logical pixels
    pub least: (f32, f32),
}

impl Placement {
    pub(crate) fn new(limits: Limits) -> Rc<Self> {
        Rc::new(Placement {
            limits,
            preferred: Cell::new(limits.preferred.map(|(w, h)| (w as f64, h as f64))),
            auto: RefCell::new(Vec::new()),
            scale: Cell::new(0.0),
            shown: Cell::new(None),
            held: Cell::new(false),
            room: Cell::new(None),
            pending: Cell::new(Cause::Nothing),
            drifted: Cell::new(false),
            moves: Cell::new(0),
            candidate: Cell::new(None),
            watching: Cell::new(false),
            quiet_until: Cell::new(None),
        })
    }

    /// The size the person wants, in logical pixels (the one it has when this first asks, unless the limits say).
    pub(crate) fn preferred(&self, seen: &Seen) -> (f64, f64) {
        if self.preferred.get().is_none() {
            self.preferred.set(Some((seen.inner.0 as f64 / seen.scale, seen.inner.1 as f64 / seen.scale)));
        }
        self.preferred.get().unwrap_or_default()
    }

    /// The inside the work area leaves it, as last found (physical pixels).
    pub(crate) fn room(&self) -> Option<(u32, u32)> {
        self.room.get()
    }

    /// Notes a size this window was given on its own (not the person's), taken at the scale it was last measured at.
    pub(crate) fn note_auto(&self, size: (u32, u32)) {
        let scale = if self.scale.get() > 0.0 { self.scale.get() } else { 1.0 };
        let mut auto = self.auto.borrow_mut();
        auto.push((size.0 as f64 / scale, size.1 as f64 / scale));
        let extra = auto.len().saturating_sub(AUTO_KEPT);
        auto.drain(..extra);
    }

    /// Whether `inner` at `scale` is a size this set lately, give or take what rounding between physical and logical pixels does.
    fn is_auto(&self, inner: (u32, u32), scale: f64) -> bool {
        self.auto.borrow().iter().any(|&(w, h)| {
            let (w, h) = physical((w, h), scale);
            w.abs_diff(inner.0) <= ROUNDING && h.abs_diff(inner.1) <= ROUNDING
        })
    }

    /// Learns what the person wants from the window as it is now: a size they dragged it to, not one the system or this code made, or one a look that no drag started (`learns` false) found.
    #[cfg(test)]
    pub(crate) fn observe(&self, seen: &Seen, restored: bool) {
        self.observe_at(seen, restored, true, Instant::now());
    }

    fn observe_at(&self, seen: &Seen, restored: bool, learns: bool, now: Instant) {
        let first = self.preferred.get().is_none();
        self.preferred(seen);
        if first || seen.held {
            self.scale.set(seen.scale);
            return;
        }
        if self.scale.replace(seen.scale) != seen.scale || restored {
            self.quiet_until.set(Some(now + QUIET_AFTER_RESIZE));
            return;
        }
        // A work area the window was not fitted to is still changing, and the system resizes the window on its own meanwhile (the taskbar coming back)
        let area_changed = self.shown.get().is_some_and(|shown| shown != (seen.area, seen.scale.to_bits()));
        if !learns || area_changed || self.quiet_until.get().is_some_and(|until| now < until) || self.is_auto(seen.inner, seen.scale) {
            return;
        }
        self.preferred.set(Some((seen.inner.0 as f64 / seen.scale, seen.inner.1 as f64 / seen.scale)));
        self.auto.borrow_mut().clear();
    }

    /// Whether the work area and scale `now` are one to fit to: not the ones the window was last measured against, and the same as at the look before
    /// (the first sighting of a change is only noted).
    pub(crate) fn area_settled(&self, now: (Area, u64)) -> bool {
        if self.shown.get() == Some(now) {
            self.candidate.set(None);
            return false;
        }
        if self.candidate.get() == Some(now) {
            self.candidate.set(None);
            return true;
        }
        self.candidate.set(Some(now));
        false
    }

    /// How the window should be now, and what it has been measured against.
    pub(crate) fn plan(&self, seen: &Seen, mv: Move) -> Plan {
        self.plan_at(seen, mv, Instant::now())
    }

    fn plan_at(&self, seen: &Seen, mv: Move, now: Instant) -> Plan {
        let preferred = self.preferred(seen);
        if self.scale.get() > 0.0 && self.scale.get() != seen.scale {
            self.quiet_until.set(Some(now + QUIET_AFTER_RESIZE));
        }
        self.scale.set(seen.scale);
        self.shown.set(Some((seen.area, seen.scale.to_bits())));
        let margin = if seen.exact { (self.limits.margin as f64 * seen.scale).round() as u32 } else { 0 };
        let room = (seen.area.width.saturating_sub(seen.frame.0 + margin), seen.area.height.saturating_sub(seen.frame.1 + margin));
        self.room.set(Some(room));
        let least = self.limits.least_for((room.0 as f32 / seen.scale as f32, room.1 as f32 / seen.scale as f32));
        let fit = decide(seen, preferred, &self.limits, mv);
        if let Some(size) = fit.and_then(|f| f.size) {
            self.note_auto(size);
            self.quiet_until.set(Some(now + QUIET_AFTER_RESIZE));
        }
        Plan { fit, least }
    }

    /// `plan`, with the window's minimum set to what it found and the fit applied. Whether the size had to change.
    fn settle(&self, ui: &ComponentInstance, seen: &Seen, mv: Move) -> bool {
        let plan = self.plan(seen, mv);
        if self.limits.lowers_least {
            let _ = ui.set_property("least-width", Value::Number(plan.least.0 as f64));
            let _ = ui.set_property("least-height", Value::Number(plan.least.1 as f64));
        }
        apply(ui, plan.fit);
        plan.fit.is_some_and(|f| f.size.is_some())
    }
}

/// Where a window of outer size `child` starts to sit in the middle of a parent whose frame starts at `parent` and measures `parent_size`.
pub(crate) fn centred_over(parent: (i32, i32), parent_size: (u32, u32), child: (u32, u32)) -> (i32, i32) {
    let middle = |at: i32, outer: u32, inner: u32| at.saturating_add((outer as i64 - inner as i64).div_euclid(2) as i32);
    (middle(parent.0, parent_size.0, child.0), middle(parent.1, parent_size.1, child.1))
}

/// Puts the window in the middle of its parent once the system has made it, cut down to the parent's screen (the one it is about to be on, not the one
/// it was made on) and kept inside it. Where the system won't say where the parent is (Wayland), the compositor's own placement stays.
pub(crate) fn centre_over(ui: &ComponentInstance, parent: &ComponentInstance, place: &Rc<Placement>) {
    let parent = parent.as_weak();
    let place = place.clone();
    when_made(ui, move |w| {
        let seen = measure(w)?;
        let parent = parent.upgrade()?;
        let found = parent.window().with_winit_window(|p| {
            let (area, exact) = work_area(p)?;
            Some((p.outer_position().ok().map(|at| ((at.x, at.y), p.outer_size())), area, exact, p.scale_factor()))
        });
        Some((seen, found.flatten()))
    }, move |ui, (seen, parent)| {
        let Some((Some((at, size)), area, exact, scale)) = parent else {
            place.settle(ui, &seen, Move::Yes);
            return;
        };
        // The window as it will be on the parent's screen
        let by = scale / seen.scale;
        let scaled = |(w, h): (u32, u32)| ((w as f64 * by).round() as u32, (h as f64 * by).round() as u32);
        let there = Seen { position: None, inner: scaled(seen.inner), frame: scaled(seen.frame), area, exact, scale, straddles: false, ..seen };
        let plan = place.plan(&there, Move::No);
        if place.limits.lowers_least {
            let _ = ui.set_property("least-width", Value::Number(plan.least.0 as f64));
            let _ = ui.set_property("least-height", Value::Number(plan.least.1 as f64));
        }
        let inner = plan.fit.and_then(|f| f.size).unwrap_or(there.inner);
        if let Some(size) = plan.fit.and_then(|f| f.size) {
            let logical = slint::LogicalSize::new((size.0 as f64 / scale) as f32, (size.1 as f64 / scale) as f32);
            if by == 1.0 { ui.window().set_size(slint::PhysicalSize::new(size.0, size.1)) } else { ui.window().set_size(logical) }
        }
        let outer = (inner.0 + there.frame.0, inner.1 + there.frame.1);
        let centred = centred_over(at, (size.width, size.height), outer);
        let seat = Seen { position: Some(centred), inner, ..there };
        let moved = decide(&seat, (inner.0 as f64 / scale, inner.1 as f64 / scale), &place.limits, Move::Yes).and_then(|f| f.position).unwrap_or(centred);
        ui.window().set_position(slint::PhysicalPosition::new(moved.0, moved.1));
    });
}

/// The work area of the screen the window is on, and whether the system said so: where it didn't, it is the whole screen.
pub(crate) fn work_area(w: &winit::window::Window) -> Option<(Area, bool)> {
    #[cfg(windows)]
    if let Some(area) = windows_work_area(w) {
        return Some((area, true));
    }
    let monitor = w.current_monitor().or_else(|| w.primary_monitor())?;
    let (at, size) = (monitor.position(), monitor.size());
    let screen = Area { x: at.x, y: at.y, width: size.width, height: size.height };
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(area) = x11_work_area(w, screen) {
        return Some((area, true));
    }
    Some((screen, false))
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

/// The part of `screen` that _NET_WORKAREA leaves on `desktop` (it lists x, y, width, height per desktop for the whole root window, panels taken out).
/// None where the list has no such desktop or nothing of the screen is left.
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn net_work_area(listed: &[i64], desktop: usize, screen: Area) -> Option<Area> {
    let rects = listed.as_chunks::<4>().0;
    let rect = rects.get(desktop).or_else(|| rects.first())?;
    let (left, top) = (rect[0].max(screen.x as i64), rect[1].max(screen.y as i64));
    let (right, bottom) = ((rect[0] + rect[2]).min(screen.x as i64 + screen.width as i64), (rect[1] + rect[3]).min(screen.y as i64 + screen.height as i64));
    (right > left && bottom > top).then(|| Area { x: left as i32, y: top as i32, width: (right - left) as u32, height: (bottom - top) as u32 })
}

/// X11: _NET_WORKAREA on the root window, cut to the screen the window is on. None on Wayland or where the window manager doesn't say.
#[cfg(all(unix, not(target_os = "macos")))]
fn x11_work_area(w: &winit::window::Window, screen: Area) -> Option<Area> {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    use x11_dl::xlib;
    thread_local! {
        static XLIB: Option<xlib::Xlib> = xlib::Xlib::open().ok();
    }
    let RawDisplayHandle::Xlib(display) = w.display_handle().ok()?.as_raw() else { return None };
    let display = display.display?.as_ptr() as *mut xlib::Display;
    XLIB.with(|x| {
        let x = x.as_ref()?;
        // The root window's property of this name as numbers (what a format of 32 bits is given as: `long`s)
        let cardinals = |name: &std::ffi::CStr| -> Option<Vec<i64>> {
            // SAFETY: the display is the one winit is using on this thread; the property's memory is read for the count it says and then freed
            unsafe {
                let atom = (x.XInternAtom)(display, name.as_ptr(), xlib::True);
                if atom == 0 {
                    return None;
                }
                let root = (x.XDefaultRootWindow)(display);
                let (mut kind, mut format, mut count, mut after, mut data) = (0, 0, 0, 0, std::ptr::null_mut());
                let status = (x.XGetWindowProperty)(display, root, atom, 0, 4096, xlib::False, xlib::XA_CARDINAL, &mut kind, &mut format, &mut count, &mut after, &mut data);
                let found = (status == 0 && kind == xlib::XA_CARDINAL && format == 32 && !data.is_null())
                    .then(|| std::slice::from_raw_parts(data as *const std::ffi::c_long, count as usize).to_vec());
                if !data.is_null() {
                    (x.XFree)(data.cast());
                }
                found
            }
        };
        let desktop = cardinals(c"_NET_CURRENT_DESKTOP").and_then(|d| d.first().copied()).unwrap_or(0).max(0) as usize;
        net_work_area(&cardinals(c"_NET_WORKAREA")?, desktop, screen)
    })
}

/// How the window sits on its screen right now. None until the system has made the window, which happens on a turn of the event loop after it
/// was shown (and again with a window that has no size yet).
pub(crate) fn measure(w: &winit::window::Window) -> Option<Seen> {
    let (inner, outer) = (w.inner_size(), w.outer_size());
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let frame = (outer.width.saturating_sub(inner.width), outer.height.saturating_sub(inner.height));
    let (area, exact) = work_area(w)?;
    let position = w.outer_position().ok().map(|p| (p.x, p.y));
    let held = w.is_maximized() || w.is_minimized() == Some(true) || w.fullscreen().is_some();
    let straddles = position.is_some_and(|at| {
        let here = w.current_monitor();
        let others: Vec<Area> = w.available_monitors().filter(|m| Some(m) != here.as_ref())
            .map(|m| Area { x: m.position().x, y: m.position().y, width: m.size().width, height: m.size().height }).collect();
        reaches_onto(at, (outer.width, outer.height), &others)
    });
    Some(Seen { position, inner: (inner.width, inner.height), frame, area, exact, scale: w.scale_factor(), held, straddles })
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
pub(crate) fn keep_inside(ui: &ComponentInstance, place: &Rc<Placement>) {
    let place = place.clone();
    when_made(ui, measure, move |ui, seen| {
        place.settle(ui, &seen, Move::Yes);
    });
}

fn apply(ui: &ComponentInstance, fit: Option<Fit>) {
    let Some(fit) = fit else { return };
    let window = ui.window();
    if let Some((width, height)) = fit.size {
        window.set_size(slint::PhysicalSize::new(width, height));
    }
    if let Some((x, y)) = fit.position {
        window.set_position(slint::PhysicalPosition::new(x, y));
    }
}

/// The fit for a screen that is not there (tests, where no system window exists): the window as it is, taken as `scale` makes it, on a work area of
/// `area` physical pixels with a frame of `frame`. Sets the size it comes to and gives it.
pub(crate) fn fit_by_hand(ui: &ComponentInstance, place: &Rc<Placement>, area: Area, scale: f64, frame: (u32, u32)) -> (u32, u32) {
    let logical = ui.window().size().to_logical(ui.window().scale_factor());
    let inner = physical((logical.width as f64, logical.height as f64), scale);
    let seen = Seen { position: None, inner, frame, area, exact: true, scale, held: false, straddles: false };
    let plan = place.plan(&seen, Move::No);
    if place.limits.lowers_least {
        let _ = ui.set_property("least-width", Value::Number(plan.least.0 as f64));
        let _ = ui.set_property("least-height", Value::Number(plan.least.1 as f64));
    }
    let size = plan.fit.and_then(|f| f.size).unwrap_or(inner);
    ui.window().set_size(slint::LogicalSize::new((size.0 as f64 / scale) as f32, (size.1 as f64 / scale) as f32));
    size
}

/// Why a window is looked at again.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Cause {
    Nothing,
    /// It was resized: what the person drags it to is what they want
    Resized,
    /// It moved: only a move onto another screen matters, and then it is cut down without being moved, so a drag isn't fought
    Moved,
    /// Its screen's work area changed with nothing moving (a taskbar, a Wayland output): cut down and moved back in
    Display,
    /// The scale changed (another screen's, or the display setting): cut down and moved back in
    Scale,
}

/// How many of its own sizes a window remembers: a resize event can arrive after the next fit was already made.
const AUTO_KEPT: usize = 4;
/// How far (physical pixels) a size may be from one this set and still be that one, for the rounding between physical and logical pixels.
const ROUNDING: u32 = 2;

/// How long after it was resized (by this code, or by the system for a new scale) the sizes a window reports are not taken for its person's.
const QUIET_AFTER_RESIZE: Duration = Duration::from_millis(600);
/// How long after a fit is made the window is checked to have the size it was given.
const RECHECK: Duration = Duration::from_millis(700);
/// How long a work area that just changed is left before it is looked at again: a taskbar that slides in or out passes through sizes on the way.
const LOOK_AGAIN: Duration = Duration::from_millis(250);
/// How long a burst of events is left to settle before the window is looked at.
const SETTLE: Duration = Duration::from_millis(60);
/// How long a window that moved is left still before the drag is taken to have ended.
const QUIET: Duration = Duration::from_millis(300);
/// How often a visible window's work area is looked at, for the changes no event tells (a taskbar moved, a Wayland output changed).
const POLL: Duration = Duration::from_secs(1);

/// Keeps a window inside its screen's work area for as long as it lives: what it hangs off the window's winit events (see
/// keys::watch_scan_codes_and). A scale change, a move onto a screen with another work area (and, once the drag ends, back inside it) or a change of the
/// work area itself fits it again, the same way as at its first showing, from the size the person wants and not the one an earlier cut left.
pub(crate) fn follow(ui: &ComponentInstance, place: &Rc<Placement>) -> impl FnMut(&winit::event::WindowEvent) + 'static {
    use winit::event::WindowEvent;
    let (weak, place) = (ui.as_weak(), place.clone());
    let poll = slint::Timer::default();
    {
        let (weak, place) = (weak.clone(), place.clone());
        poll.start(slint::TimerMode::Repeated, POLL, move || look_at_area(weak.clone(), place.clone()));
    }
    move |event| {
        // The timer lives as long as the window's filter does
        let _ = &poll;
        let cause = match event {
            WindowEvent::ScaleFactorChanged { .. } => Cause::Scale,
            WindowEvent::Moved(_) => {
                place.moves.set(place.moves.get() + 1);
                Cause::Moved
            }
            WindowEvent::Resized(_) => Cause::Resized,
            _ => return,
        };
        // A look is already on its way: it learns of the stronger cause
        if place.pending.replace(place.pending.get().max(cause)) != Cause::Nothing {
            return;
        }
        let (weak, place) = (weak.clone(), place.clone());
        slint::Timer::single_shot(SETTLE, move || {
            let cause = place.pending.replace(Cause::Nothing);
            if let Some(ui) = weak.upgrade() {
                refit(&ui, &place, cause);
            }
        });
    }
}

/// A look at the work area and scale a visible window is on: a change is fitted to once it has been seen twice running (a second look follows soon).
fn look_at_area(weak: slint::Weak<ComponentInstance>, place: Rc<Placement>) {
    let Some(ui) = weak.upgrade() else { return };
    if !ui.window().is_visible() || place.pending.get() != Cause::Nothing || place.shown.get().is_none() {
        return;
    }
    let Some(now) = ui.window().with_winit_window(|w| work_area(w).map(|(area, _)| (area, w.scale_factor().to_bits()))).flatten() else { return };
    if place.area_settled(now) {
        refit(&ui, &place, Cause::Display);
    } else if place.candidate.get().is_some() && !place.watching.replace(true) {
        slint::Timer::single_shot(LOOK_AGAIN, move || {
            place.watching.set(false);
            look_at_area(weak, place);
        });
    }
}

/// Registers `follow` on a window that has no other watcher of its winit events.
pub(crate) fn watch(ui: &ComponentInstance, place: &Rc<Placement>) {
    let mut follow = follow(ui, place);
    ui.window().on_winit_window_event(move |_, event| {
        follow(event);
        slint::winit_030::EventResult::Propagate
    });
}

fn refit(ui: &ComponentInstance, place: &Rc<Placement>, cause: Cause) {
    let Some(seen) = ui.window().with_winit_window(measure).flatten() else { return };
    if seen.held {
        place.held.set(true);
        return;
    }
    let restored = place.held.replace(false);
    place.observe_at(&seen, restored, matches!(cause, Cause::Resized | Cause::Moved), Instant::now());
    let changed = place.shown.get() != Some((seen.area, seen.scale.to_bits()));
    let (act, mv) = match cause {
        Cause::Nothing => return,
        Cause::Resized => (restored, Move::Yes),
        Cause::Moved => (changed || restored, if restored { Move::Yes } else { Move::No }),
        Cause::Display | Cause::Scale => (true, Move::Yes),
    };
    if act && place.settle(ui, &seen, mv) {
        recheck(ui, place);
    }
    if cause == Cause::Moved && (changed || place.drifted.get()) {
        place.drifted.set(true);
        end_of_drag(ui, place, place.moves.get());
    }
}

/// Looks once more at a window that was just resized: the system may have set another size over ours (a scale change does), and the window is fitted again
/// to the size its person wants.
fn recheck(ui: &ComponentInstance, place: &Rc<Placement>) {
    let (weak, place) = (ui.as_weak(), place.clone());
    slint::Timer::single_shot(RECHECK, move || {
        let Some(ui) = weak.upgrade() else { return };
        if place.pending.get() != Cause::Nothing || crate::input::button_down(ui.window()) == Some(true) {
            return;
        }
        let Some(seen) = ui.window().with_winit_window(measure).flatten() else { return };
        // A work area that moved on since is the next look's
        if !seen.held && place.shown.get() == Some((seen.area, seen.scale.to_bits())) {
            place.settle(&ui, &seen, Move::UnlessStraddling);
        }
    });
}

/// Once the moves have stopped and no button is down, puts the window back inside its screen: a tall one dropped on a shorter screen was cut down on its way
/// there, and what it still hangs over is brought in.
fn end_of_drag(ui: &ComponentInstance, place: &Rc<Placement>, moves: u64) {
    let (weak, place) = (ui.as_weak(), place.clone());
    slint::Timer::single_shot(QUIET, move || {
        let Some(ui) = weak.upgrade() else { return };
        if place.moves.get() != moves {
            return;
        }
        if crate::input::button_down(ui.window()) == Some(true) {
            return end_of_drag(&ui, &place, moves);
        }
        place.drifted.set(false);
        let Some(seen) = ui.window().with_winit_window(measure).flatten() else { return };
        if !seen.held {
            place.settle(&ui, &seen, Move::UnlessStraddling);
        }
    });
}


#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = Area { x: 0, y: 0, width: 1280, height: 752 };
    const FRAME: (u32, u32) = (16, 39);

    fn seen(position: Option<(i32, i32)>, inner: (u32, u32), frame: (u32, u32), area: Area, scale: f64) -> Seen {
        Seen { position, inner, frame, area, exact: true, scale, held: false, straddles: false }
    }

    /// A window that wants the size it has, fitted as the first look does.
    fn fit_scaled(position: Option<(i32, i32)>, inner: (u32, u32), frame: (u32, u32), area: Area, least: (f32, f32), scale: f64) -> Option<Fit> {
        decide(&seen(position, inner, frame, area, scale), (inner.0 as f64 / scale, inner.1 as f64 / scale), &Limits::new(least), Move::Yes)
    }

    fn fit_in(position: Option<(i32, i32)>, inner: (u32, u32), frame: (u32, u32), area: Area, least: (f32, f32)) -> Option<Fit> {
        fit_scaled(position, inner, frame, area, least, 1.0)
    }

    #[test]
    fn a_window_bigger_than_the_screen_is_cut_down_and_moved_in() {
        // What the VM showed: 1380x900 inside, opened at 104,104 on a 1280x800 screen with a taskbar of 48
        let fit = fit_in(Some((104, 104)), (1380, 900), FRAME, SCREEN, (560.0, 520.0)).unwrap();
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
        assert_eq!(fit_in(Some((100, 50)), (900, 600), FRAME, SCREEN, (560.0, 520.0)), None);
    }

    #[test]
    fn a_window_that_fits_but_sticks_out_is_moved_not_resized() {
        let fit = fit_in(Some((600, 300)), (900, 600), FRAME, SCREEN, (560.0, 520.0)).unwrap();
        assert_eq!(fit, Fit { position: Some((1280 - 916, 752 - 639)), size: None });
    }

    #[test]
    fn the_taskbar_is_left_alone_wherever_the_work_area_starts() {
        let top_bar = Area { x: 1920, y: 40, width: 1280, height: 720 };
        let fit = fit_in(Some((1900, 0)), (1380, 900), FRAME, top_bar, (560.0, 520.0)).unwrap();
        assert_eq!(fit, Fit { position: Some((1920, 40)), size: Some((1264, 681)) });
    }

    #[test]
    fn a_window_whose_position_is_unknown_is_only_cut_down() {
        let fit = fit_in(None, (1380, 900), FRAME, SCREEN, (560.0, 520.0)).unwrap();
        assert_eq!(fit, Fit { position: None, size: Some((1264, 713)) });
    }

    #[test]
    fn a_small_screen_never_takes_it_under_its_minimum() {
        let small = Area { x: 0, y: 0, width: 500, height: 400 };
        let fit = fit_in(Some((0, 0)), (1380, 900), FRAME, small, (560.0, 520.0)).unwrap();
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

    fn px((w, h): (f32, f32), scale: f64) -> (u32, u32) {
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
        let fit = fit_scaled(Some((244, 0)), px(SETTINGS, 1.5), WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, 1.5).unwrap();
        assert_eq!(fit, Fit { position: None, size: Some((1410, 976)) });
    }

    #[test]
    fn a_taskbar_at_the_top_moves_the_window_below_it() {
        let fit = fit_scaled(Some((244, 0)), px(SETTINGS, 1.5), WINDOWS_FRAME, bar("top"), SETTINGS_LEAST, 1.5).unwrap();
        assert_eq!(fit, Fit { position: Some((244, 48)), size: Some((1410, 976)) });
    }

    #[test]
    fn a_taskbar_at_the_left_or_right_cuts_the_width_and_keeps_clear_of_it() {
        let main = px(MAIN, 1.5);
        assert_eq!(main, (2070, 1350));
        let left = fit_scaled(Some((0, 0)), main, (16, 39), bar("left"), MAIN_LEAST, 1.5).unwrap();
        assert_eq!(left, Fit { position: Some((48, 0)), size: Some((1856, 1041)) });
        let right = fit_scaled(Some((60, 0)), main, (16, 39), bar("right"), MAIN_LEAST, 1.5).unwrap();
        assert_eq!(right, Fit { position: Some((0, 0)), size: Some((1856, 1041)) });
    }

    #[test]
    fn the_main_window_at_125_percent_is_cut_to_the_work_area() {
        let main = px(MAIN, 1.25);
        assert_eq!(main, (1725, 1125));
        let fit = fit_scaled(Some((0, 0)), main, (16, 39), bar("bottom"), MAIN_LEAST, 1.25).unwrap();
        assert_eq!(fit, Fit { position: None, size: Some((1725, 993)) });
    }

    #[test]
    fn what_fits_at_one_scale_is_cut_when_the_scale_grows() {
        // 1100x700 logical is fine at 100% and 125%, and 150% makes it 1650x1050 inside on a work area of 1032 high
        let at = |scale: f64| fit_scaled(Some((100, 40)), px((1100.0, 700.0), scale), WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, scale);
        assert_eq!(at(1.0), None);
        assert_eq!(at(1.25), None);
        assert_eq!(at(1.5), Some(Fit { position: Some((100, 0)), size: Some((1650, 976)) }));
    }

    #[test]
    fn a_window_is_not_made_bigger_than_it_wants_to_be() {
        let small = px((700.0, 500.0), 1.5);
        assert_eq!(fit_scaled(Some((10, 10)), small, WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, 1.5), None);
        assert_eq!(fit_scaled(None, small, WINDOWS_FRAME, bar("bottom"), SETTINGS_LEAST, 1.5), None);
    }

    #[test]
    fn a_work_area_smaller_than_the_minimum_leaves_the_minimum() {
        // A tiny screen: the minimum (in logical pixels, at the scale) wins
        let tiny = Area { x: 0, y: 0, width: 700, height: 500 };
        let fit = fit_scaled(Some((0, 0)), px(MAIN, 1.5), (16, 39), tiny, MAIN_LEAST, 1.5).unwrap();
        assert_eq!(fit.size, Some((840, 780)), "560x520 at 150%");
        assert_eq!(fit.position, None, "its corner and title bar stay on the screen");
        let fit = fit_scaled(Some((0, 0)), px(MAIN, 1.25), (16, 39), tiny, MAIN_LEAST, 1.25).unwrap();
        assert_eq!(fit.size, Some((700, 650)), "560x520 at 125%");
    }

    #[test]
    fn the_least_size_is_logical_so_it_grows_with_the_scale() {
        let area = Area { x: 0, y: 0, width: 1000, height: 700 };
        let at = |scale: f64| fit_scaled(None, px(MAIN, scale), (16, 39), area, MAIN_LEAST, scale).unwrap().size.unwrap();
        assert_eq!(at(1.0), (984, 661));
        assert_eq!(at(1.25), (984, 661));
        assert_eq!(at(1.5), (984, 780), "the height is cut no further than 520 logical");
    }

    #[test]
    fn a_stronger_cause_wins_when_several_arrive_together() {
        assert!(Cause::Scale > Cause::Display && Cause::Display > Cause::Moved && Cause::Moved > Cause::Resized && Cause::Resized > Cause::Nothing);
    }

    #[test]
    fn a_maximized_minimized_or_full_screen_window_is_left_to_the_system() {
        // A scale change that makes the work area smaller than what the system gave it: it stays as the system has it, however it is cut
        let held = Seen { held: true, ..seen(Some((-11, -11)), (1920, 1009), (22, 56), bar("bottom"), 1.5) };
        assert_eq!(decide(&held, (1380.0, 900.0), &Limits::main(), Move::Yes), None);
        let place = Placement::new(Limits::main());
        assert_eq!(place.plan(&held, Move::Yes).fit, None);
        // And not taken for the person resizing it
        place.observe(&held, false);
        assert_eq!(place.preferred(&held), (1920.0 / 1.5, 1009.0 / 1.5), "what it had when first asked, not the maximized size twice");
        place.observe(&Seen { inner: (700, 500), ..held }, false);
        assert_eq!(place.preferred(&held), (1920.0 / 1.5, 1009.0 / 1.5));
    }

    #[test]
    fn a_window_grows_back_to_what_its_person_wants_on_a_bigger_screen() {
        let place = Placement::new(Limits::main());
        // Opened at 1380x900 on a 1080p screen at 100% with a taskbar: cut to the room
        let first = seen(Some((0, 0)), px(MAIN, 1.0), (16, 39), Area { x: 0, y: 0, width: 1920, height: 1032 }, 1.0);
        assert_eq!(place.plan(&first, Move::Yes).fit, None);
        // Dragged to a 1366x728 screen: cut
        let small = Area { x: 1920, y: 0, width: 1366, height: 728 };
        let cut = place.plan(&Seen { position: Some((1920, 0)), area: small, ..first }, Move::No).fit.unwrap();
        assert_eq!(cut.size, Some((1350, 689)));
        // Dragged back: it is what it was, not the size it was cut to
        let cut_window = Seen { inner: (1350, 689), area: Area { x: 0, y: 0, width: 1920, height: 1032 }, ..first };
        place.observe(&cut_window, false);
        let back = place.plan(&cut_window, Move::No).fit.unwrap();
        assert_eq!(back.size, Some((1380, 900)));
    }

    #[test]
    fn a_size_the_person_drags_it_to_is_what_they_want_from_then_on() {
        let place = Placement::new(Limits::main());
        let first = seen(Some((0, 0)), px(MAIN, 1.0), (16, 39), bar("bottom"), 1.0);
        place.plan(&first, Move::Yes);
        // Its own cut is not the person's
        let cut = Seen { inner: (1000, 700), ..first };
        place.note_auto((1000, 700));
        place.observe(&cut, false);
        assert_eq!(place.preferred(&first), (1380.0, 900.0));
        // A drag to 1100x640 is, and so is one that lands where a cut once did
        place.observe(&Seen { inner: (1100, 640), ..first }, false);
        assert_eq!(place.preferred(&first), (1100.0, 640.0));
        place.observe(&Seen { inner: (1000, 700), ..first }, false);
        assert_eq!(place.preferred(&first), (1000.0, 700.0));
        // A new scale resizes it without the person: the logical size is kept
        place.observe(&Seen { inner: (1500, 1050), scale: 1.5, ..first }, false);
        assert_eq!(place.preferred(&first), (1000.0, 700.0));
    }

    /// A window on a screen with a taskbar that hides and comes back, as Windows treats it: the size asked for is the size it gets, give or take `jitter`.
    struct Hiding {
        place: Rc<Placement>,
        scale: f64,
        inner: (u32, u32),
        /// The time the watcher sees, which only `wait` moves on
        clock: Instant,
    }

    impl Hiding {
        const SHOWN: Area = Area { x: 0, y: 0, width: 1920, height: 1020 };
        const HIDDEN: Area = Area { x: 0, y: 0, width: 1920, height: 1080 };

        fn open(scale: f64) -> Self {
            let place = Placement::new(Limits::main());
            let mut window = Hiding { place, scale, inner: px(MAIN, scale), clock: Instant::now() };
            window.fit(Self::SHOWN, 0);
            window.arrives();
            window
        }

        fn seen(&self, area: Area) -> Seen {
            seen(Some((0, 0)), self.inner, (16, 39), area, self.scale)
        }

        /// The work area changes: the window is fitted and takes the size asked for, off by `jitter` pixels
        fn fit(&mut self, area: Area, jitter: i32) {
            if let Some(size) = self.place.plan_at(&self.seen(area), Move::Yes, self.clock).fit.and_then(|f| f.size) {
                self.inner = (size.0.saturating_add_signed(jitter), size.1.saturating_add_signed(jitter));
            }
        }

        /// The resize event of the size it has now reaches the watcher
        fn arrives(&self) {
            self.place.observe_at(&self.seen(Self::SHOWN), false, true, self.clock);
        }

        /// The same, as the work area is by then `area`
        fn arrives_on(&self, area: Area) {
            self.place.observe_at(&self.seen(area), false, true, self.clock);
        }

        fn wait(&mut self, time: Duration) {
            self.clock += time;
        }

        fn wants(&self) -> (f64, f64) {
            self.place.preferred(&self.seen(Self::SHOWN))
        }
    }

    #[test]
    fn a_taskbar_that_hides_and_returns_does_not_shrink_what_the_window_grows_back_to() {
        for scale in [1.0, 1.25, 1.5] {
            let mut window = Hiding::open(scale);
            let wanted = window.wants();
            assert_eq!(wanted, (1380.0, 900.0));
            window.fit(Hiding::HIDDEN, 0);
            let grown = window.inner;
            window.arrives();
            window.fit(Hiding::SHOWN, 0);
            window.arrives();
            for _ in 0..3 {
                window.fit(Hiding::HIDDEN, 0);
                assert_eq!(window.inner, grown, "at {scale}");
                window.arrives();
                window.fit(Hiding::SHOWN, 0);
                window.arrives();
                assert_eq!(window.wants(), wanted, "at {scale}");
            }
        }
    }

    #[test]
    fn resize_events_that_arrive_after_the_next_fit_are_still_its_own() {
        for scale in [1.25, 1.5] {
            let mut window = Hiding::open(scale);
            let wanted = window.wants();
            // The bar hides and returns before the window has been told of its first resize
            window.fit(Hiding::HIDDEN, 0);
            let grown = window.inner;
            window.fit(Hiding::SHOWN, 0);
            let cut = window.inner;
            assert_ne!(grown, cut, "at {scale}");
            for inner in [grown, cut] {
                window.inner = inner;
                window.arrives();
                assert_eq!(window.wants(), wanted, "at {scale}");
            }
            window.fit(Hiding::HIDDEN, 0);
            assert_eq!(window.inner, grown, "it grows back to what it was, at {scale}");
        }
    }

    #[test]
    fn a_size_off_by_a_pixel_from_the_one_asked_for_is_not_the_persons() {
        for scale in [1.25, 1.5, 1.75] {
            for jitter in [-2, -1, 1, 2] {
                let mut window = Hiding::open(scale);
                let wanted = window.wants();
                window.fit(Hiding::HIDDEN, jitter);
                window.arrives();
                window.fit(Hiding::SHOWN, jitter);
                window.arrives();
                assert_eq!(window.wants(), wanted, "at {scale} off by {jitter}");
            }
        }
        // A real drag still is
        let mut window = Hiding::open(1.5);
        window.fit(Hiding::SHOWN, 0);
        window.inner = (1500, 900);
        window.wait(Duration::from_secs(2));
        window.arrives();
        assert_eq!(window.wants(), (1000.0, 600.0));
    }

    #[test]
    fn a_work_area_that_is_still_changing_is_not_fitted_to_until_it_looks_the_same_twice() {
        let place = Placement::new(Limits::main());
        let at = |area: Area| (area, 1.25f64.to_bits());
        place.plan(&seen(Some((0, 0)), px(MAIN, 1.25), (16, 39), Hiding::SHOWN, 1.25), Move::Yes);
        let sliding = Area { height: 1052, ..Hiding::SHOWN };
        assert!(!place.area_settled(at(Hiding::SHOWN)), "nothing changed");
        assert!(!place.area_settled(at(sliding)), "seen once on its way");
        assert!(!place.area_settled(at(Hiding::HIDDEN)), "another size, so the first is not the one it stopped at");
        assert!(place.area_settled(at(Hiding::HIDDEN)), "the same again");
        // A change that goes back to what it was is not one
        assert!(!place.area_settled(at(sliding)));
        assert!(!place.area_settled(at(Hiding::SHOWN)));
        assert!(!place.area_settled(at(Hiding::SHOWN)));
        assert!(!place.area_settled(at(sliding)), "its first sighting again");
    }

    #[test]
    fn a_size_the_system_reports_while_a_work_area_change_settles_is_not_the_persons() {
        // Windows at 125%: the bar going away grows the window to the whole screen, and coming back cuts it, with a size in between on the way
        let mut window = Hiding::open(1.25);
        let wanted = window.wants();
        window.wait(Duration::from_secs(5));
        window.fit(Hiding::HIDDEN, 0);
        let grown = window.inner;
        window.arrives();
        window.wait(Duration::from_secs(5));
        window.fit(Hiding::SHOWN, 0);
        let cut = window.inner;
        window.wait(Duration::from_millis(100));
        for between in [(cut.0, cut.1 + 8), (cut.0, cut.1 + 3)] {
            window.inner = between;
            window.arrives();
            assert_eq!(window.wants(), wanted, "{between:?}");
        }
        // The check that follows puts it where it belongs
        window.wait(RECHECK);
        window.fit(Hiding::SHOWN, 0);
        assert_eq!(window.inner, cut);
        window.arrives();
        window.wait(Duration::from_secs(5));
        window.fit(Hiding::HIDDEN, 0);
        assert_eq!(window.inner, grown, "the next time the bar goes it grows as far as the first time did");
    }

    #[test]
    fn a_size_the_system_sets_before_the_new_work_area_is_fitted_to_is_not_the_persons() {
        // Windows at 125% and 150%: the bar coming back resizes the window while the last fit was still to the bigger work area, long after any quiet time
        for (scale, system) in [(1.25, (1725, 990)), (1.5, (1920, 1046))] {
            let mut window = Hiding::open(scale);
            let wanted = window.wants();
            window.wait(Duration::from_secs(5));
            window.fit(Hiding::HIDDEN, 0);
            let grown = window.inner;
            window.wait(Duration::from_secs(5));
            window.inner = system;
            window.arrives_on(Hiding::SHOWN);
            assert_eq!(window.wants(), wanted, "at {scale}");
            // The fit that follows and the next time the bar goes are as before
            window.fit(Hiding::SHOWN, 0);
            window.arrives();
            window.wait(Duration::from_secs(5));
            window.fit(Hiding::HIDDEN, 0);
            assert_eq!(window.inner, grown, "at {scale}");
        }
    }

    #[test]
    fn a_work_area_or_scale_change_is_never_a_drag() {
        let mut window = Hiding::open(1.25);
        let wanted = window.wants();
        window.wait(Duration::from_secs(5));
        window.inner = (1500, 800);
        window.place.observe_at(&window.seen(Hiding::SHOWN), false, false, window.clock);
        assert_eq!(window.wants(), wanted);
    }

    #[test]
    fn a_live_scale_change_ends_at_the_size_a_fresh_launch_at_that_scale_gives() {
        let (high, low) = (Area { x: 0, y: 0, width: 1920, height: 1008 }, Area { x: 0, y: 0, width: 1920, height: 1020 });
        let frame = |scale: f64| if scale == 1.5 { (22, 56) } else { (18, 48) };
        let on = |scale: f64, inner: (u32, u32)| seen(Some((0, 0)), inner, frame(scale), if scale == 1.5 { high } else { low }, scale);
        let fresh = |scale: f64| Placement::new(Limits::main()).plan(&on(scale, px(MAIN, scale)), Move::Yes).fit.unwrap().size.unwrap();
        let (fresh_150, fresh_125) = (fresh(1.5), fresh(1.25));
        let place = Placement::new(Limits::main());
        let mut at = Instant::now();
        let mut inner = px(MAIN, 1.5);
        inner = place.plan_at(&on(1.5, inner), Move::Yes, at).fit.unwrap().size.unwrap();
        assert_eq!(inner, fresh_150);
        // The display goes to 125% and the system gives the window a size of its own (1618x919 outside on the Windows laptop), then a look at the new scale fits it
        at += Duration::from_secs(10);
        let system = (1600, 871);
        place.observe_at(&on(1.25, system), false, false, at);
        let fitted = place.plan_at(&on(1.25, system), Move::Yes, at).fit.unwrap().size.unwrap();
        assert_eq!(fitted, fresh_125);
        // The system sets its size over ours a moment later, and tells of it
        at += Duration::from_millis(80);
        place.observe_at(&on(1.25, system), false, true, at);
        assert_eq!(place.preferred(&on(1.25, system)), (1380.0, 900.0));
        // The check that follows grows it back
        at += RECHECK;
        assert_eq!(place.plan_at(&on(1.25, system), Move::UnlessStraddling, at).fit.unwrap().size, Some(fresh_125));
        // And back at 150% it is what it was
        at += Duration::from_secs(10);
        place.observe_at(&on(1.5, fresh_125), false, false, at);
        assert_eq!(place.plan_at(&on(1.5, fresh_125), Move::Yes, at).fit.unwrap().size, Some(fresh_150));
    }

    #[test]
    fn a_window_restored_after_a_scale_change_while_maximized_goes_back_to_what_its_person_wants() {
        let mut window = Hiding::open(1.25);
        let wanted = window.wants();
        // Maximized at 125%, the scale changes to 150% under it, and restoring gives it whatever size the system remembered
        let held = |window: &Hiding, scale: f64| Seen { held: true, scale, ..window.seen(Hiding::SHOWN) };
        window.place.observe(&held(&window, 1.25), false);
        window.place.observe(&held(&window, 1.5), false);
        window.scale = 1.5;
        window.inner = (1618, 919);
        window.place.observe(&window.seen(Hiding::SHOWN), true);
        assert_eq!(window.wants(), wanted);
        window.fit(Hiding::SHOWN, 0);
        assert_eq!(window.inner, (1904, 981), "the room a work area of 1920x1020 leaves at 150%, not the size the system gave it");
    }

    #[test]
    fn a_tall_window_dropped_low_on_a_shorter_screen_is_pulled_up_once_the_drag_ends() {
        let place = Placement::new(Limits::new(MAIN_LEAST));
        let short = Area { x: 0, y: 0, width: 1366, height: 728 };
        let dropped = seen(Some((300, 200)), (1100, 700), FRAME, short, 1.0);
        place.plan(&Seen { area: bar("bottom"), position: Some((300, 0)), ..dropped }, Move::Yes);
        // While it is dragged it is cut and left where the person has it
        let during = place.plan(&dropped, Move::No).fit.unwrap();
        assert_eq!(during, Fit { position: None, size: Some((1100, 689)) });
        // Then, with the cut made, what still hangs over the bottom is brought in
        let cut = Seen { inner: (1100, 689), ..dropped };
        assert_eq!(place.plan(&cut, Move::UnlessStraddling).fit, Some(Fit { position: Some((250, 0)), size: None }));
        // Not one placed across two screens
        assert_eq!(place.plan(&Seen { straddles: true, ..cut }, Move::UnlessStraddling).fit, None);
    }

    #[test]
    fn a_frame_that_reaches_onto_another_screen_straddles() {
        let others = [Area { x: 1920, y: 0, width: 1366, height: 768 }];
        assert!(reaches_onto((1500, 100), (600, 400), &others));
        assert!(!reaches_onto((1000, 100), (920, 400), &others), "touching is not reaching");
        assert!(!reaches_onto((100, 100), (600, 400), &others));
        assert!(!reaches_onto((100, 100), (600, 400), &[]));
    }

    #[test]
    fn a_work_area_that_shrinks_cuts_and_moves_the_window_in() {
        // A taskbar that comes up (or moves to the top) with the window and the screen unchanged
        let place = Placement::new(Limits::new(SETTINGS_LEAST));
        let before = seen(Some((100, 20)), px((1100.0, 1000.0), 1.0), WINDOWS_FRAME, Area { x: 0, y: 0, width: 1920, height: 1080 }, 1.0);
        assert_eq!(place.plan(&before, Move::Yes).fit, None);
        let after = Seen { area: bar("top"), ..before };
        assert_eq!(place.plan(&after, Move::Yes).fit, Some(Fit { position: Some((100, 48)), size: Some((1100, 976)) }));
        // And once it goes again, it is what it was
        let back = Seen { inner: (1100, 976), ..before };
        place.observe(&back, false);
        assert_eq!(place.plan(&back, Move::Yes).fit, Some(Fit { position: None, size: Some((1100, 1000)) }));
    }

    #[test]
    fn where_the_work_area_is_not_known_a_window_that_fits_the_screen_keeps_its_size() {
        // Wayland: the compositor has fitted it above its panel and the output is all we know, so it is neither cut by a guess nor made bigger than it was given
        let screen = Area { x: 0, y: 0, width: 1920, height: 1080 };
        let place = Placement::new(Limits::settings());
        let given = Seen { exact: false, ..seen(None, (940, 600), (0, 0), screen, 1.0) };
        assert_eq!(place.plan(&given, Move::Yes).fit, None, "not grown to 940x680, not cut to the output less its margin");
        let crowded = Seen { exact: false, ..seen(None, (940, 1060), (0, 0), screen, 1.0) };
        assert_eq!(place.plan(&crowded, Move::Yes).fit, None, "a window that fits the whole output is not a guess to cut");
        let huge = Seen { exact: false, ..seen(None, (2200, 1300), (0, 0), screen, 1.0) };
        assert_eq!(place.plan(&huge, Move::Yes).fit, Some(Fit { position: None, size: Some((1920, 1080)) }), "one that cannot fit anyway is cut to the output");
        // Where the work area is known, the same window is cut to it, clear of the edges
        let known = Seen { exact: true, ..crowded };
        assert_eq!(place.plan(&known, Move::Yes).fit.unwrap().size, Some((940, 680)), "it wants 940x680");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn x11_says_the_work_area_for_the_whole_root_window_and_each_screen_takes_its_part() {
        // Two desktops; a panel of 30 at the top of the root window, which is 3286 wide
        let listed = [0, 30, 3286, 1050, 0, 0, 3286, 1080];
        let first = Area { x: 0, y: 0, width: 1920, height: 1080 };
        let second = Area { x: 1920, y: 0, width: 1366, height: 768 };
        assert_eq!(net_work_area(&listed, 0, first), Some(Area { x: 0, y: 30, width: 1920, height: 1050 }));
        assert_eq!(net_work_area(&listed, 0, second), Some(Area { x: 1920, y: 30, width: 1366, height: 738 }));
        assert_eq!(net_work_area(&listed, 1, first), Some(first), "the other desktop has no panel");
        assert_eq!(net_work_area(&listed, 7, first), Some(Area { x: 0, y: 30, width: 1920, height: 1050 }), "an unknown desktop reads as the first");
        assert_eq!(net_work_area(&[0, 0, 100, 100], 0, second), None, "nothing of the screen is left");
        assert_eq!(net_work_area(&[], 0, first), None);
    }

    #[test]
    fn the_main_window_lowers_its_minimum_to_a_short_screen_and_not_below_its_floor() {
        // 1366x768 at 150%: 910x512 logical, less a taskbar of 40 and a title bar of 56 physical pixels
        let area = Area { x: 0, y: 0, width: 1366, height: 728 };
        let place = Placement::new(Limits::main());
        let window = seen(Some((0, 0)), px(MAIN, 1.5), (22, 56), area, 1.5);
        let plan = place.plan(&window, Move::Yes);
        assert_eq!(plan.least, (560.0, 448.0), "the height it gets from a screen with 448 left, in logical pixels");
        assert_eq!(plan.fit.unwrap().size, Some((1344, 672)));
        let tiny = Seen { area: Area { x: 0, y: 0, width: 800, height: 400 }, ..window };
        assert_eq!(place.plan(&tiny, Move::Yes).least, (560.0, 320.0), "the floor");
        let tall = Seen { area: bar("bottom"), scale: 1.0, ..window };
        assert_eq!(place.plan(&tall, Move::Yes).least, (560.0, 520.0), "the usual one where the room is there");
    }

    #[test]
    fn settings_keeps_clear_of_the_edges_and_its_minimum_gives_way_on_a_small_screen() {
        let place = Placement::new(Limits::settings());
        let open = |area: Area, inner: (u32, u32)| place.plan(&seen(None, inner, (0, 0), area, 1.0), Move::No);
        let large = open(Area { x: 0, y: 0, width: 1920, height: 1080 }, (940, 680));
        assert_eq!((large.fit, large.least), (None, (720.0, 480.0)));
        let small = open(Area { x: 0, y: 0, width: 800, height: 500 }, (940, 680));
        assert_eq!(small.fit.unwrap().size, Some((752, 452)));
        assert_eq!(small.least, (720.0, 452.0));
    }

    #[test]
    fn a_window_that_keeps_its_shape_is_cut_by_the_side_that_is_short() {
        let limits = Limits::shaped((160.0, 90.0));
        let screen = Area { x: 0, y: 0, width: 1366, height: 400 };
        let fit = decide(&seen(Some((0, 0)), (1280, 720), (16, 39), screen, 1.0), (1280.0, 720.0), &limits, Move::Yes).unwrap();
        assert_eq!(fit.size, Some((642, 361)), "16:9 inside the 361 left");
    }
}
