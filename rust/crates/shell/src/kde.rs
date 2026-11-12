//! KDE Plasma's title bars in the top bar's colours. Plasma draws a window's title bar from a colour scheme file and takes a different
//! one per window when the window names it: on Wayland through org_kde_kwin_server_decoration_palette, on X11 through the
//! _KDE_NET_WM_COLOR_SCHEME property. Anything that doesn't know either (GNOME, other compositors) keeps its own title bar.
//!
//! The file is named after its contents, because KWin ignores a window that names the path it already has: another look is another
//! path. There are as many files as looks, so nothing needs cleaning up.

use slint_interpreter::{ComponentInstance, Value};
use std::io;
use std::path::{Path, PathBuf};

use slint::winit_030::winit;

/// The four colours a title bar is drawn from, as red, green and blue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Scheme {
    /// The top bar's colour, which the title bar wears in both states
    pub bar: [u8; 3],
    /// The text on it, and the buttons
    pub ink: [u8; 3],
    /// The text of a window that isn't the active one
    pub quiet: [u8; 3],
    pub accent: [u8; 3],
}

impl Scheme {
    /// What the window's look has now: None if the Theme isn't there to read.
    pub(crate) fn of(ui: &ComponentInstance) -> Option<Scheme> {
        let colour = |name: &str| match ui.get_global_property("Theme", name) {
            Ok(Value::Brush(b)) => {
                let c = b.color();
                Some([c.red(), c.green(), c.blue()])
            }
            _ => None,
        };
        Some(Scheme { bar: colour("chrome")?, ink: colour("ink")?, quiet: colour("ink2")?, accent: colour("accent")? })
    }

    /// The .colors file: [WM] is what KWin reads for the bar, the Window and Header groups are what Breeze draws its buttons from.
    pub(crate) fn colors(&self) -> String {
        let rgb = |c: [u8; 3]| format!("{},{},{}", c[0], c[1], c[2]);
        let (bar, ink, quiet, accent) = (rgb(self.bar), rgb(self.ink), rgb(self.quiet), rgb(self.accent));
        let group = |name: &str, foreground: &str| {
            format!("[{name}]\nBackgroundNormal={bar}\nBackgroundAlternate={bar}\nForegroundNormal={foreground}\nForegroundInactive={quiet}\nDecorationFocus={accent}\nDecorationHover={accent}\n\n")
        };
        format!(
            "[General]\nColorScheme=Telescope\n\n[WM]\nactiveBackground={bar}\nactiveForeground={ink}\nactiveBlend={ink}\ninactiveBackground={bar}\ninactiveForeground={quiet}\ninactiveBlend={quiet}\n\n{}{}{}",
            group("Colors:Window", &ink),
            group("Colors:Header", &ink),
            group("Colors:Header][Inactive", &quiet),
        )
    }
}

/// FNV-1a: the same text gives the same name in every build, which a std hasher doesn't promise.
fn fingerprint(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100000001b3))
}

/// $XDG_CACHE_HOME/telescope/titlebar, or ~/.cache/telescope/titlebar.
fn cache_dir() -> Option<PathBuf> {
    let from = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = from("XDG_CACHE_HOME").or_else(|| from("HOME").map(|h| h.join(".cache")))?;
    Some(base.join("telescope").join("titlebar"))
}

/// Writes the scheme's file into `dir` unless it's there already, and gives its absolute path. The file appears whole or not at all
/// (written beside, then renamed in): KWin may read it the moment it's named.
pub(crate) fn publish(dir: &Path, scheme: &Scheme) -> io::Result<PathBuf> {
    let text = scheme.colors();
    let path = dir.join(format!("telescope-{:016x}.colors", fingerprint(&text)));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return Ok(path);
    }
    std::fs::create_dir_all(dir)?;
    let beside = dir.join(format!(".telescope-{}.tmp", std::process::id()));
    let written = std::fs::write(&beside, &text).and_then(|_| std::fs::rename(&beside, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&beside);
    }
    written.map(|_| path)
}

/// Gives the window's title bar the scheme's colours, once the system has made the window, and again when the look changes.
pub(crate) fn dress(ui: &ComponentInstance) {
    let (Some(scheme), Some(dir)) = (Scheme::of(ui), cache_dir()) else { return };
    let path = match publish(&dir, &scheme) {
        Ok(path) => path,
        Err(e) => return log::debug!("title bar colours: {}: {e}", dir.display()),
    };
    crate::screen::when_made(ui, move |w| apply(w, &path), |_, ()| {});
}

/// None while the window has no surface yet.
fn apply(w: &winit::window::Window, path: &Path) -> Option<()> {
    use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    let (Ok(window), Ok(display)) = (w.window_handle(), w.display_handle()) else { return Some(()) };
    match (window.as_raw(), display.as_raw()) {
        (RawWindowHandle::Wayland(_), RawDisplayHandle::Wayland(display)) => wayland::paint(w, display.display.as_ptr(), path),
        (RawWindowHandle::Xlib(window), RawDisplayHandle::Xlib(display)) => {
            if let Some(display) = display.display {
                x11_scheme(display.as_ptr().cast(), window.window, path);
            }
            Some(())
        }
        _ => Some(()),
    }
}

/// _KDE_NET_WM_COLOR_SCHEME: the file's path as a STRING, which KWin reads when the property changes.
fn x11_scheme(display: *mut x11_dl::xlib::Display, window: std::ffi::c_ulong, path: &Path) {
    thread_local! {
        static XLIB: Option<x11_dl::xlib::Xlib> = x11_dl::xlib::Xlib::open().ok();
    }
    let Some(path) = path.to_str() else { return };
    XLIB.with(|x| {
        let Some(x) = x else { return };
        // SAFETY: display and window are winit's, alive while this window is; the bytes outlive the call at the length passed
        unsafe {
            let atom = (x.XInternAtom)(display, c"_KDE_NET_WM_COLOR_SCHEME".as_ptr(), 0);
            (x.XChangeProperty)(display, window, atom, x11_dl::xlib::XA_STRING, 8, x11_dl::xlib::PropModeReplace, path.as_ptr(), path.len() as i32);
            (x.XFlush)(display);
        }
    });
}

/// One palette per window surface, made over winit's own display on a connection of its own, as parent.rs does for the parent and the
/// activation: the surfaces are winit's, wrapped from its proxies, and the only events read here are the registry's.
mod wayland {
    use super::winit;
    use std::cell::{OnceCell, RefCell};
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::protocol::{wl_registry::{self, WlRegistry}, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols_plasma::server_decoration_palette::client::{
        org_kde_kwin_server_decoration_palette::OrgKdeKwinServerDecorationPalette,
        org_kde_kwin_server_decoration_palette_manager::OrgKdeKwinServerDecorationPaletteManager as Manager,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[derive(Default)]
    struct State {
        manager: Option<Manager>,
    }

    impl Dispatch<WlRegistry, ()> for State {
        fn event(state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, queue: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, .. } = event {
                if interface == Manager::interface().name {
                    state.manager = Some(registry.bind(name, 1, queue, ()));
                }
            }
        }
    }

    impl Dispatch<Manager, ()> for State {
        fn event(_: &mut Self, _: &Manager, _: <Manager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<OrgKdeKwinServerDecorationPalette, ()> for State {
        fn event(_: &mut Self, _: &OrgKdeKwinServerDecorationPalette, _: <OrgKdeKwinServerDecorationPalette as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    struct Palette {
        surface: WlSurface,
        object: OrgKdeKwinServerDecorationPalette,
        path: PathBuf,
    }

    struct Painter {
        connection: Connection,
        queue: EventQueue<State>,
        manager: Manager,
        palettes: RefCell<Vec<Palette>>,
    }

    thread_local! {
        /// None once it's known the compositor has no palette manager (anything but Plasma)
        static PAINTER: OnceCell<Option<Painter>> = const { OnceCell::new() };
    }

    fn surface(connection: &Connection, w: &winit::window::Window) -> Option<WlSurface> {
        let RawWindowHandle::Wayland(handle) = w.window_handle().ok()?.as_raw() else { return None };
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }.ok()?;
        WlSurface::from_id(connection, id).ok()
    }

    /// Some(()) when it's done or can't be done on this compositor, None while the window has no surface.
    pub(super) fn paint(w: &winit::window::Window, display: *mut c_void, path: &Path) -> Option<()> {
        PAINTER.with(|cell| {
            let painter = cell.get_or_init(|| {
                let connection = Connection::from_backend(unsafe { Backend::from_foreign_display(display.cast()) });
                let mut queue = connection.new_event_queue();
                let mut state = State::default();
                let registry = connection.display().get_registry(&queue.handle(), ());
                let _ = queue.roundtrip(&mut state);
                // Nothing dispatches this queue again, so the registry goes or every later output change piles up in it
                let _ = connection.backend().destroy_object(&registry.id());
                state.manager.map(|manager| Painter { connection, queue, manager, palettes: RefCell::default() })
            });
            let Some(painter) = painter else { return Some(()) };
            let surface = surface(&painter.connection, w)?;
            let mut palettes = painter.palettes.borrow_mut();
            // A window that was hidden took its surface with it, and its palette goes now: the object is of no use to anyone
            palettes.retain(|p| {
                let alive = p.surface.is_alive();
                if !alive {
                    p.object.release();
                }
                alive
            });
            match palettes.iter_mut().find(|p| p.surface == surface) {
                Some(have) if have.path == path => {}
                Some(have) => {
                    have.object.set_palette(path.to_string_lossy().into_owned());
                    have.path = path.to_path_buf();
                }
                None => {
                    let object = painter.manager.create(&surface, &painter.queue.handle(), ());
                    object.set_palette(path.to_string_lossy().into_owned());
                    palettes.push(Palette { surface, object, path: path.to_path_buf() });
                }
            }
            painter.connection.flush().ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW: Scheme = Scheme { bar: [20, 20, 20], ink: [238, 238, 238], quiet: [217, 217, 217], accent: [170, 156, 245] };
    const CLASSIC: Scheme = Scheme { bar: [18, 21, 25], ink: [231, 234, 239], quiet: [161, 171, 184], accent: [170, 156, 245] };

    fn folder(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telescope-kde-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_title_bar_takes_the_top_bars_colour_and_ink() {
        let text = NEW.colors();
        for line in ["activeBackground=20,20,20", "activeForeground=238,238,238", "inactiveBackground=20,20,20", "inactiveForeground=217,217,217"] {
            assert!(text.lines().any(|l| l == line), "{line} in\n{text}");
        }
        let header = text.split("[Colors:Header]\n").nth(1).unwrap();
        assert!(header.starts_with("BackgroundNormal=20,20,20\nBackgroundAlternate=20,20,20\nForegroundNormal=238,238,238\n"), "{header}");
        assert!(text.contains("[Colors:Header][Inactive]\nBackgroundNormal=20,20,20\nBackgroundAlternate=20,20,20\nForegroundNormal=217,217,217\n"), "{text}");
    }

    #[test]
    fn classic_has_its_own_blue_black_bar() {
        let text = CLASSIC.colors();
        assert!(text.contains("activeBackground=18,21,25\n") && text.contains("activeForeground=231,234,239\n"), "{text}");
        assert!(!text.contains("20,20,20"), "{text}");
    }

    #[test]
    fn a_look_has_a_path_of_its_own_and_the_same_look_the_same_one() {
        let dir = folder("names");
        let (a, b) = (publish(&dir, &NEW).unwrap(), publish(&dir, &CLASSIC).unwrap());
        assert_ne!(a, b, "KWin ignores a path it already has");
        assert_eq!(publish(&dir, &NEW).unwrap(), a);
        assert!(a.is_absolute() && a.extension().is_some_and(|e| e == "colors"), "{a:?}");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), NEW.colors());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), CLASSIC.colors());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_is_there_whole_and_nothing_is_left_beside_it() {
        let dir = folder("atomic");
        let path = publish(&dir, &NEW).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert_eq!(path.file_name().unwrap().to_string_lossy(), names.remove(0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_is_written_again_and_a_good_one_is_left_alone() {
        let dir = folder("damaged");
        let path = publish(&dir, &NEW).unwrap();
        std::fs::write(&path, "half").unwrap();
        assert_eq!(publish(&dir, &NEW).unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), NEW.colors());
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        publish(&dir, &NEW).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_that_cant_be_written_is_an_error_not_a_panic() {
        let dir = folder("blocked");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        std::fs::write(&file, "").unwrap();
        assert!(publish(&file.join("inside"), &NEW).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_scheme_comes_from_the_themes_looks() {
        i_slint_backend_testing::init_no_event_loop();
        let doc = "import { Theme } from \"@ui-kit/ui-kit.slint\";\nexport { Theme }\nexport component Probe inherits Window { }\n";
        let ui = crate::compile::compile("probe.slint", doc, vec![], "Probe").unwrap().create().unwrap();
        assert_eq!(Scheme::of(&ui), Some(NEW));
        ui.set_global_property("Theme", "look", Value::Number(1.0)).unwrap();
        assert_eq!(Scheme::of(&ui), Some(CLASSIC));
        ui.set_global_property("Theme", "look", Value::Number(2.0)).unwrap();
        assert_eq!(Scheme::of(&ui).map(|s| s.bar), Some([0, 0, 0]), "OLED's bar is black");
        ui.set_global_property("Theme", "look", Value::Number(3.0)).unwrap();
        let light = Scheme::of(&ui).unwrap();
        assert!(light.bar.iter().all(|c| *c > 200) && light.ink.iter().all(|c| *c < 60), "Light has a pale bar and dark ink: {light:?}");
    }
}
