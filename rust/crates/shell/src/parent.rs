//! A side window that stays above the main window, as a Qt dialog with a parent does (Settings, a plugin's own window, a question).
//! Windows owns it, X11 marks it transient for the main window and Wayland sets its toplevel's parent. winit has a call for the
//! first only, so the other two go through what its windows hand out: the Xlib display and the toplevel's `wl_proxy`.

use slint::winit_030::{winit, WinitWindowAccessor};
use slint::ComponentHandle;
use slint_interpreter::ComponentInstance;

#[cfg(windows)]
thread_local! {
    static OWNER: std::cell::Cell<Option<isize>> = const { std::cell::Cell::new(None) };
}

/// The backend every window of the program is made through, so one that's owned can be made owned (Windows takes the owner
/// when the window is made and not after).
pub(crate) fn selector() -> slint::BackendSelector {
    let selector = slint::BackendSelector::new();
    #[cfg(windows)]
    let selector = selector.with_winit_window_attributes_hook(|attributes| {
        use winit::platform::windows::WindowAttributesExtWindows;
        match OWNER.with(|o| o.get()) {
            Some(hwnd) => attributes.with_owner_window(hwnd),
            None => attributes,
        }
    });
    selector
}

/// Makes the window `make` creates one that parent owns; only Windows needs it done at this point, the rest is `attach`.
pub(crate) fn make<T>(parent: &ComponentInstance, make: impl FnOnce() -> T) -> T {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        // None while the parent has no window yet (it started in the tray), and the new one is then free
        let hwnd = parent.window().with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        });
        OWNER.with(|o| o.set(hwnd.flatten()));
        let made = make();
        OWNER.with(|o| o.set(None));
        made
    }
    #[cfg(not(windows))]
    {
        let _ = parent;
        make()
    }
}

/// Puts a shown window above its parent for as long as both are open. Again after each show: on Wayland a hidden window's
/// toplevel is gone, and so is the parent link of a window whose own parent was hidden.
pub(crate) fn attach(child: &ComponentInstance, parent: &ComponentInstance) {
    if !try_attach(child, parent) {
        retry(child.as_weak(), parent.as_weak(), 100);
    }
}

/// False while either has no window yet: a shown window is made on the next turn of the event loop.
fn try_attach(child: &ComponentInstance, parent: &ComponentInstance) -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    return child.window().with_winit_window(|c| parent.window().with_winit_window(|p| attach_windows(c, p))).flatten().is_some();
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (child, parent);
        true
    }
}

fn retry(child: slint::Weak<ComponentInstance>, parent: slint::Weak<ComponentInstance>, left: u32) {
    slint::Timer::single_shot(std::time::Duration::from_millis(10), move || {
        let (Some(c), Some(p)) = (child.upgrade(), parent.upgrade()) else { return };
        // Closed since, or the parent never comes up (it's in the tray): nothing to wait for
        if left > 0 && c.window().is_visible() && !try_attach(&c, &p) {
            retry(child, parent, left - 1);
        }
    });
}

/// raise_ and activateWindow on an owned window: not minimized, with the keyboard. It's above its parent already. Wayland's
/// focus_window does nothing, so the window is asked for through xdg_activation by the one that has the keyboard now, its parent.
pub(crate) fn raise(child: &ComponentInstance, parent: &ComponentInstance) {
    child.window().set_minimized(false);
    child.window().with_winit_window(|c| {
        c.focus_window();
        #[cfg(all(unix, not(target_os = "macos")))]
        parent.window().with_winit_window(|p| activate_windows(c, p));
    });
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = parent;
}

#[cfg(all(unix, not(target_os = "macos")))]
fn attach_windows(child: &winit::window::Window, parent: &winit::window::Window) -> Option<()> {
    use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    let (Ok(window), Ok(display)) = (child.window_handle(), child.display_handle()) else { return Some(()) };
    match (window.as_raw(), display.as_raw()) {
        (RawWindowHandle::Wayland(_), RawDisplayHandle::Wayland(display)) => wayland::set_parent(child, parent, display.display.as_ptr()),
        (RawWindowHandle::Xlib(child), RawDisplayHandle::Xlib(display)) => {
            if let (Some(display), Ok(RawWindowHandle::Xlib(parent))) = (display.display, parent.window_handle().map(|h| h.as_raw())) {
                x11_transient(display.as_ptr().cast(), child.window, parent.window);
            }
            Some(())
        }
        _ => Some(()),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn activate_windows(window: &winit::window::Window, active: &winit::window::Window) {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    if let Ok(RawDisplayHandle::Wayland(display)) = window.display_handle().map(|h| h.as_raw()) {
        wayland::activate(window, active, display.display.as_ptr());
    }
}

/// WM_TRANSIENT_FOR, which window managers keep a window above its parent by.
#[cfg(all(unix, not(target_os = "macos")))]
fn x11_transient(display: *mut x11_dl::xlib::Display, child: std::ffi::c_ulong, parent: std::ffi::c_ulong) {
    thread_local! {
        static XLIB: Option<x11_dl::xlib::Xlib> = x11_dl::xlib::Xlib::open().ok();
    }
    XLIB.with(|x| {
        if let Some(x) = x {
            unsafe {
                (x.XSetTransientForHint)(display, child, parent);
                (x.XFlush)(display);
            }
        }
    });
}

/// What winit has no call for: xdg_toplevel.set_parent, and xdg_activation to give a window the keyboard. The toplevels and
/// surfaces are winit's, wrapped from its proxies, and the requests go out over its display on a connection of their own that
/// sends and reads nothing but its own few events, so winit's stay winit's.
#[cfg(all(unix, not(target_os = "macos")))]
mod wayland {
    use super::winit;
    use std::cell::{OnceCell, RefCell};
    use std::ffi::c_void;
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::protocol::{wl_registry::{self, WlRegistry}, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols::xdg::activation::v1::client::{xdg_activation_token_v1::{self, XdgActivationTokenV1}, xdg_activation_v1::XdgActivationV1};
    use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
    use winit::platform::wayland::WindowExtWayland;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[derive(Default)]
    struct State {
        activation: Option<XdgActivationV1>,
        token: Option<String>,
    }

    impl Dispatch<WlRegistry, ()> for State {
        fn event(state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, queue: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, .. } = event {
                if interface == XdgActivationV1::interface().name {
                    state.activation = Some(registry.bind(name, 1, queue, ()));
                }
            }
        }
    }

    impl Dispatch<XdgActivationV1, ()> for State {
        fn event(_: &mut Self, _: &XdgActivationV1, _: <XdgActivationV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<XdgActivationTokenV1, ()> for State {
        fn event(state: &mut Self, _: &XdgActivationTokenV1, event: xdg_activation_token_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            let xdg_activation_token_v1::Event::Done { token } = event else { return };
            state.token = Some(token);
        }
    }

    struct Sender {
        connection: Connection,
        queue: RefCell<EventQueue<State>>,
        state: RefCell<State>,
    }

    thread_local! {
        static SENDER: OnceCell<Sender> = const { OnceCell::new() };
    }

    fn with_sender<R>(display: *mut c_void, f: impl FnOnce(&Sender) -> Option<R>) -> Option<R> {
        SENDER.with(|cell| {
            let sender = cell.get_or_init(|| {
                let connection = Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
                let mut queue = connection.new_event_queue();
                let mut state = State::default();
                connection.display().get_registry(&queue.handle(), ());
                let _ = queue.roundtrip(&mut state);
                Sender { connection, queue: RefCell::new(queue), state: RefCell::new(state) }
            });
            f(sender)
        })
    }

    fn toplevel(sender: &Sender, w: &winit::window::Window) -> Option<XdgToplevel> {
        let id = unsafe { ObjectId::from_ptr(XdgToplevel::interface(), w.xdg_toplevel()?.as_ptr().cast()) }.ok()?;
        XdgToplevel::from_id(&sender.connection, id).ok()
    }

    fn surface(sender: &Sender, w: &winit::window::Window) -> Option<WlSurface> {
        let RawWindowHandle::Wayland(handle) = w.window_handle().ok()?.as_raw() else { return None };
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }.ok()?;
        WlSurface::from_id(&sender.connection, id).ok()
    }

    /// None while either window has no toplevel yet.
    pub(super) fn set_parent(child: &winit::window::Window, parent: &winit::window::Window, display: *mut c_void) -> Option<()> {
        with_sender(display, |s| {
            toplevel(s, child)?.set_parent(Some(&toplevel(s, parent)?));
            s.connection.flush().ok()
        })
    }

    /// Asks the compositor to give `window` the keyboard, on the word of `active`, which has it. The compositor decides: a token
    /// from a window that's active is what it takes to hand the focus over inside one program.
    pub(super) fn activate(window: &winit::window::Window, active: &winit::window::Window, display: *mut c_void) -> Option<()> {
        with_sender(display, |s| {
            let activation = s.state.borrow().activation.clone()?;
            let (target, from) = (surface(s, window)?, surface(s, active)?);
            let token = activation.get_activation_token(&s.queue.borrow().handle(), ());
            token.set_surface(&from);
            token.commit();
            s.state.borrow_mut().token = None;
            let _ = s.queue.borrow_mut().roundtrip(&mut s.state.borrow_mut());
            let given = s.state.borrow_mut().token.take();
            token.destroy();
            activation.activate(given?, &target);
            s.connection.flush().ok()
        })
    }
}
