//! The window: builds the document from the registered plugins (doc.rs), wires its callbacks back to them and carries
//! out what they ask for (shell::Effect). Streams go through the stream flow (streams.rs).

use crate::doc::{self, CardPlace, HeaderPlace, Piece};
use crate::layout::Region;
use crate::shell::{Effect, State};
use crate::source::{scan, to_json, to_property};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use slint_interpreter::{ComponentInstance, Struct, Value as SValue};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use telescope_core::config::ConfigFile;
use telescope_core::event::Event;
use telescope_core::i18n::tr;
use telescope_core::link::{RoutePrefs, StreamRequest};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::services::Services;
use telescope_core::source::Phones;
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::{BusSender, Frame, PixelFormat};

static APP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The binary says which desktop entry its windows belong to before they open (autostart::APP_ID, on a dev copy with -dev).
pub fn use_app_id(id: String) {
    let _ = APP_ID.set(id);
}

/// Called once when the window has been shown (or put in the tray), before the event loop runs.
#[derive(Clone)]
pub struct ShownHook(pub Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for ShownHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShownHook")
    }
}

/// How the window streams, from the command line.
#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub prefs: RoutePrefs,
    pub request: StreamRequest,
    pub hardware_decode: bool,
    /// The phone app USB pairing offers go to.
    pub package: String,
    /// Start streaming once the window is up.
    pub autostart: bool,
    pub quit_after: Option<u64>,
    /// Save the window as a PPM here once it has drawn.
    pub snapshot: Option<String>,
    /// --minimized: start in the tray (minimized where there's none).
    pub minimized: bool,
    /// Runs once the window is up, for the app to start counting towards confirming an update.
    pub on_shown: Option<ShownHook>,
}

/// Which element of the document a plugin's piece of UI is.
struct PieceRef {
    plugin: String,
    ui: String,
    id: String,
}

pub struct Window {
    pub state: Rc<RefCell<State>>,
    pub ui: ComponentInstance,
    pieces: Vec<PieceRef>,
    fold_version: Cell<i32>,
    save_timer: slint::Timer,
    pub(crate) services: Option<Arc<Services>>,
    pub(crate) opts: Option<WindowOptions>,
    pub streams: crate::streams::Streams,
    /// Plugins' pending host.schedule timers, by (plugin, token).
    timers: RefCell<std::collections::BTreeMap<(String, String), slint::Timer>>,
    /// Draws the preview's frames: on Slint's device once it says which, else on the CPU (preview.rs).
    painter: RefCell<Option<Box<dyn crate::preview::Painter>>>,
    preview_timer: slint::Timer,
    /// The number of the frame the preview shows, so a frame isn't painted twice.
    painted: Cell<u64>,
    /// What passes on the preview (tint, analyse, paint) have in hand.
    passes: RefCell<crate::preview_passes::PassWork>,
    /// The layers' shapes last handed to the preview card, so they're only set again when they change.
    layers_drawn: RefCell<(Vec<crate::overlays::LayerPath>, Vec<crate::overlays::LayerText>)>,
    /// The first frame shown has been written to the log.
    logged: Cell<bool>,
    /// The preview's pop-out window while it's open, and the card's popout-asks it last answered.
    popout: RefCell<Option<ComponentInstance>>,
    cadence: RefCell<crate::preview::Cadence>,
    /// It's been fitted to the screen, which it only is as it first opens.
    pub(crate) fitted: Cell<bool>,
    /// Windows plugins opened of their own (HostUi::open_window): (plugin, id, window).
    pub(crate) plugin_windows: RefCell<Vec<(String, String, ComponentInstance)>>,
    popout_asks: Cell<i32>,
    painted_gen: Cell<i32>,
}

/// _PopoutWindow: the picture alone on black, in a window of its own.
const POPOUT: &str = r#"
import { PreviewMarks, PreviewPick, LensOutline, SkeletonLine } from "shell/preview.slint";

export struct LayerPath { commands: string, stroke: color, width: length, fill: color, evenodd: bool }
export struct LayerText { x: length, y: length, text: string, color: color, size: length }

export component PreviewPopout inherits Window {
    title: @tr("Telescope - Video Preview");
    in property <image> app-icon;
    icon: app-icon;
    min-width: 320px;
    min-height: 180px;
    background: #000;
    in property <image> frame;
    in property <bool> has-frame;
    in property <image> overlay;
    in property <bool> has-overlay;
    in property <string> stage-cursor;
    in property <bool> marker-shown;
    in property <float> marker-u;
    in property <float> marker-v;
    in property <color> marker-colour;
    in property <[LayerPath]> layer-paths;
    in property <[LayerText]> layer-texts;
    in property <string> layer-font;
    in property <color> bone;
    in property <color> held-out;
    in property <[LensOutline]> lens-boxes;
    in property <float> boxes-opacity;
    in property <[SkeletonLine]> skeleton-lines;
    in property <bool> warped;
    in property <bool> picking;
    in property <string> pick-dim;
    in property <string> pick-line;
    in property <string> pick-handles;
    in property <string> pick-hint;
    in property <string> pick-cursor;
    property <float> aspect: has-frame && frame.height > 0 ? frame.width / frame.height : root.width / max(1px, root.height);
    out property <length> pic-width: min(root.width, root.height * aspect);
    out property <length> pic-height: pic-width / max(aspect, 0.01);
    property <length> pic-x: (root.width - pic-width) / 2;
    property <length> pic-y: (root.height - pic-height) / 2;
    callback view-pointer(string, float, float, float, float);
    callback view-wheel(float, float, float);
    callback pick-pointer(string, float, float, float, float);
    callback pick-key(string);
    if has-frame: Image {
        width: parent.width;
        height: parent.height;
        source: frame;
        image-fit: contain;
        image-rendering: smooth;
    }
    if has-frame && has-overlay: Image {
        width: parent.width;
        height: parent.height;
        source: overlay;
        image-fit: contain;
        image-rendering: smooth;
    }
    if has-frame && (layer-paths.length > 0 || layer-texts.length > 0): Rectangle {
        x: pic-x;
        y: pic-y;
        width: pic-width;
        height: pic-height;
        for shape in layer-paths: Path {
            width: parent.width;
            height: parent.height;
            viewbox-width: self.width / 1px;
            viewbox-height: self.height / 1px;
            commands: shape.commands;
            stroke: shape.stroke;
            stroke-width: shape.width;
            fill: shape.fill;
            fill-rule: shape.evenodd ? FillRule.evenodd : FillRule.nonzero;
        }
        for t in layer-texts: Text {
            x: t.x;
            y: t.y;
            text: t.text;
            color: t.color;
            font-family: layer-font;
            font-size: t.size;
        }
    }
    if has-frame && !warped: PreviewMarks {
        x: pic-x;
        y: pic-y;
        width: pic-width;
        height: pic-height;
        skeleton-lines: skeleton-lines;
        lens-boxes: lens-boxes;
        boxes-opacity: boxes-opacity;
        bone: bone;
        held-out: held-out;
        font: layer-font;
    }
    // The card's stage, as it's the same FrameLabel in Qt
    if has-frame: TouchArea {
        mouse-cursor: stage-cursor == "cross" ? MouseCursor.crosshair : stage-cursor == "grab" ? MouseCursor.grab : stage-cursor == "grabbing" ? MouseCursor.grabbing : MouseCursor.default;
        pointer-event(e) => {
            let kind = e.kind == PointerEventKind.cancel ? "cancel"
                : e.kind == PointerEventKind.move ? (self.pressed ? "move" : "")
                : e.button != PointerEventButton.left ? ""
                : e.kind == PointerEventKind.down ? "down" : e.kind == PointerEventKind.up ? "up" : "";
            if kind != "" {
                root.view-pointer(kind, (self.mouse-x - pic-x) / max(pic-width, 1px), (self.mouse-y - pic-y) / max(pic-height, 1px), pic-width / 1px, pic-height / 1px);
            }
        }
        scroll-event(e) => {
            let u = (self.mouse-x - pic-x) / max(pic-width, 1px);
            let v = (self.mouse-y - pic-y) / max(pic-height, 1px);
            if u < 0 || u > 1 || v < 0 || v > 1 || e.delta-y == 0 {
                return reject;
            }
            root.view-wheel(u, v, e.delta-y / 60px);
            accept
        }
    }
    if has-frame && marker-shown: Rectangle {
        x: pic-x + marker-u * pic-width - 22px;
        y: pic-y + marker-v * pic-height - 22px;
        width: 44px;
        height: 44px;
        border-width: 2px;
        border-color: marker-colour;
        border-radius: 6px;
    }
    if picking: PreviewPick {
        width: parent.width;
        height: parent.height;
        pic-x: pic-x;
        pic-y: pic-y;
        pic-width: pic-width;
        pic-height: pic-height;
        pick-dim: pick-dim;
        pick-line: pick-line;
        pick-handles: pick-handles;
        pick-hint: pick-hint;
        pick-cursor: pick-cursor;
        font: layer-font;
        pick-pointer(kind, u, v, w, h) => { root.pick-pointer(kind, u, v, w, h); }
        pick-key(key) => { root.pick-key(key); }
    }
}
"#;

/// The picture a window's title bar and the taskbar show (create_app_icon): the installer's icon, as pixels, since a window
/// icon has to be one and an SVG isn't.
pub(crate) fn app_icon() -> Option<slint::Image> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(include_bytes!("../ui/telescope.png").as_slice())).read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    let mut rgba = vec![0; reader.output_buffer_size()?];
    reader.next_frame(&mut rgba).ok()?;
    let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, width, height);
    Some(slint::Image::from_rgba8(pixels))
}

/// Gives a window of the shell's the app's icon.
pub(crate) fn set_app_icon(window: &ComponentInstance) {
    if let Some(icon) = app_icon() {
        let _ = window.set_property("app-icon", SValue::Image(icon));
    }
}

/// raise_ and activateWindow on an open window that has no parent: not minimized, in front and with the keyboard. show() does
/// nothing on a window that's already shown. On Wayland, focus_window does nothing and a compositor grants an activation only
/// with a fresh input serial, which winit keeps to itself, so it's opened again, which brings it up in front (and puts it back
/// where the compositor likes, so a window that has a parent goes through parent::raise instead).
pub(crate) fn bring_forward(ui: &ComponentInstance) {
    use slint::winit_030::WinitWindowAccessor;
    let window = ui.window();
    window.set_minimized(false);
    window.with_winit_window(|w| w.focus_window());
    if crate::input::on_wayland(window) {
        let _ = ui.hide();
        let _ = ui.show();
    }
}

/// _PopoutWindow.resizeEvent: a resized pop-out takes the stream's shape again, by its height, once it's off by over 4 px.
fn fit_popout(p: &ComponentInstance, frame_w: u32, frame_h: u32) {
    let size = p.window().size();
    if frame_w == 0 || frame_h == 0 || size.width == 0 {
        return;
    }
    let target = (size.width as f64 * frame_h as f64 / frame_w as f64).round() as u32;
    if target.abs_diff(size.height) > 4 {
        p.window().set_size(slint::PhysicalSize::new(size.width, target));
    }
}

thread_local! {
    static WINDOW: RefCell<Option<Rc<Window>>> = const { RefCell::new(None) };
}

/// The window on this thread, for callbacks that come back from other threads.
pub(crate) fn with_window(f: impl FnOnce(&Rc<Window>)) {
    if let Some(w) = WINDOW.with(|w| w.borrow().clone()) {
        f(&w);
    }
}

/// A picture as a Slint image, for the formats that are plain pixels.
fn image_of(frame: &Frame) -> Option<slint::Image> {
    let pixels: Vec<u8> = match frame.format {
        PixelFormat::Rgba => frame.data.to_vec(),
        PixelFormat::Bgra => frame.data.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        _ => return None,
    };
    if pixels.len() != frame.width as usize * frame.height as usize * 4 {
        return None;
    }
    Some(slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&pixels, frame.width, frame.height)))
}

/// Where a UiSpec's text comes from and the file the document imports it as.
pub(crate) fn source_of(plugin: &str, ui: &str, spec: &UiSpec) -> (String, String) {
    match &spec.source {
        SlintSource::Text(text) => (format!("plugins/{plugin}/{ui}.slint"), text.clone()),
        SlintSource::File(path) => (path.clone(), std::fs::read_to_string(path).unwrap_or_default()),
    }
}

/// A plugin's card as the window's document places it, with the file it imports. The real window and the check that an
/// added plugin's card fits in it both use this.
pub(crate) fn card_place(info: &PluginInfo, card: &CardSpec, id: &str) -> (CardPlace, (String, String)) {
    let (file, text) = source_of(&info.name, "card", &card.ui);
    let title = if info.title.is_empty() { info.name.clone() } else { info.title.clone() };
    let place = CardPlace {
        piece: Piece { id: id.into(), file: file.clone(), component: card.ui.component.clone(), iface: scan(&text, &card.ui.component) },
        region: Region::from_name(&card.region),
        fold_key: format!("{}/{}", info.name, crate::source::heading(&text, &card.ui.component).unwrap_or(title)),
        category: info.category.clone(),
    };
    (place, (file, text))
}

/// A plugin's header widget as the window's document places it, with the file it imports.
pub(crate) fn header_place(info: &PluginInfo, header: &UiSpec, id: &str) -> (HeaderPlace, (String, String)) {
    let (file, text) = source_of(&info.name, "header", header);
    let place = HeaderPlace {
        piece: Piece { id: id.into(), file: file.clone(), component: header.component.clone(), iface: scan(&text, &header.component) },
        right: info.header_side == "right",
    };
    (place, (file, text))
}

/// Registers plugins into a fresh state and builds their window. services is None for scene renders.
pub fn build(file: ConfigFile, plugins: impl FnOnce(&BusSender) -> Vec<Box<dyn Plugin>>, services: Option<Arc<Services>>,
             opts: Option<WindowOptions>) -> Result<Rc<Window>, String> {
    let mut state = State::new(file);
    state.link = services.as_ref().map(|s| s.link.clone());
    state.frames = services.as_ref().and_then(|s| s.media.stream_frames()).map(|f| f as Arc<dyn telescope_core::host::FrameStore>);
    state.vision = services.as_ref().and_then(|s| s.vision.clone());
    if let Some(admin) = services.as_ref().and_then(|s| s.plugin_admin.clone()) {
        state.admin = admin;
    }
    if let Some(vision) = &state.vision {
        vision.attach(state.bus.sender());
    }
    let plugins = plugins(&state.bus.sender());
    // Filled in by the factory just now: the added plugins that are on but couldn't start
    let problems = services.as_ref().map(|s| s.plugin_problems.lock().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or_default();
    state.set_plugin_problems(&problems);
    state.load_builtins(plugins)?;
    state.apply_saved_config();
    let phones = state.plugin_config("connection").map_or_else(
        || phones_from_file(&state.file).phones.len() as u32,
        |c| c.get("phones").and_then(Value::as_array).map_or(0, |p| p.len() as u32));
    state.bus.emit(Event::PhonesChanged { count: phones });

    let (mut cards, mut headers, mut files, mut pieces) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, slot) in state.slots.iter().enumerate() {
        let name = &slot.info.name;
        if let Some(card) = &slot.card {
            let id = format!("p{i}");
            let (place, file) = card_place(&slot.info, card, &id);
            cards.push(place);
            files.push(file);
            pieces.push(PieceRef { plugin: name.clone(), ui: "card".into(), id });
        }
        if let Some(header) = &slot.header {
            let id = format!("h{i}");
            let (place, file) = header_place(&slot.info, header, &id);
            headers.push(place);
            files.push(file);
            pieces.push(PieceRef { plugin: name.clone(), ui: "header".into(), id });
        }
    }
    // Cards are numbered by their slot, so the panels' indexes are slot indexes too
    let mut panels = state.panels.clone();
    let renumber = |list: &mut Vec<usize>| list.iter_mut().for_each(|i| *i = cards.iter().position(|c| c.piece.id == format!("p{i}")).unwrap_or(0));
    renumber(&mut panels.left);
    renumber(&mut panels.center);
    renumber(&mut panels.right);
    let text = doc::window(&cards, &panels, &headers);
    let def = crate::compile::compile("window.slint", &text, files.clone(), "TelescopeWindow")?;
    let ui = def.create().map_err(|e| e.to_string())?;
    set_app_icon(&ui);

    let window = Rc::new(Window {
        state: Rc::new(RefCell::new(state)),
        ui,
        pieces,
        fold_version: Cell::new(0),
        save_timer: slint::Timer::default(),
        services,
        opts,
        streams: crate::streams::Streams::new(),
        timers: RefCell::default(),
        painter: RefCell::default(),
        preview_timer: slint::Timer::default(),
        painted: Cell::new(0),
        passes: RefCell::default(),
        layers_drawn: RefCell::default(),
        logged: Cell::new(false),
        popout: RefCell::default(),
        cadence: RefCell::default(),
        fitted: Cell::new(false),
        plugin_windows: RefCell::default(),
        popout_asks: Cell::new(0),
        painted_gen: Cell::new(0),
    });
    WINDOW.with(|w| *w.borrow_mut() = Some(window.clone()));
    window.streams.flow.borrow_mut().slot_memory = window.state.borrow().slot_memory.clone();
    // Events emitted from other threads get delivered on this one
    window.state.borrow().bus.set_waker(|| {
        let _ = slint::invoke_from_event_loop(|| with_window(|w| w.process()));
    });
    window.state.borrow().posts.set_waker(|| {
        let _ = slint::invoke_from_event_loop(|| with_window(|w| w.process()));
    });
    // Whatever is on the clipboard now, pasted into the hidden box; an empty answer is no answer (the paste may not have worked)
    let weak = Rc::downgrade(&window);
    window.state.borrow_mut().clipboard = Some(Box::new(move || {
        let window = weak.upgrade()?;
        match window.ui.invoke("paste-text", &[]) {
            Ok(SValue::String(text)) if !text.is_empty() => Some(text.to_string()),
            _ => None,
        }
    }));
    window.wire(&files, &cards, &headers);
    window.first_props();
    window.process();
    Ok(window)
}

fn phones_from_file(file: &ConfigFile) -> Phones {
    let cfg = file.load();
    Phones::from_section(cfg.pointer("/plugin_configs/connection"), || (String::new(), String::new()))
}

impl Window {
    /// The Shortcuts global behind ui-kit's ShortcutMenu: what a control's right-click lists, and its items back to
    /// the plugin that set it (Host::set_shortcut_menu).
    fn wire_shortcut_menu(self: &Rc<Self>) {
        let state = self.state.clone();
        let _ = self.ui.set_global_callback("Shortcuts", "bindings-for", move |args| {
            let action = match args.first() { Some(SValue::String(s)) => s.to_string(), _ => String::new() };
            let st = state.borrow();
            let listed = st.shortcut_menu.iter().flat_map(|(_, b)| b).filter(|b| action.split(' ').any(|a| a == b.action))
                .map(|b| crate::source::from_json(&serde_json::json!({"id": b.id, "text": b.text}))).collect::<Vec<_>>();
            SValue::Model(slint::ModelRc::new(slint::VecModel::from(listed)))
        });
        for callback in ["add", "edit"] {
            let weak = Rc::downgrade(self);
            let _ = self.ui.set_global_callback("Shortcuts", callback, move |args| {
                let Some(w) = weak.upgrade() else { return SValue::Void };
                let Some(plugin) = w.state.borrow().shortcut_menu.as_ref().map(|(p, _)| p.clone()) else { return SValue::Void };
                let event = UiEvent { ui: "shortcut-menu".into(), callback: callback.into(), args: args.iter().map(to_json).collect() };
                w.state.borrow_mut().ui_event(&plugin, event);
                w.process();
                SValue::Void
            });
        }
    }

    /// The TextCap global behind a text field with a cap.
    fn wire_text_cap(&self) {
        let _ = self.ui.set_global_callback("TextCap", "fit", |args| match args {
            [SValue::String(old), SValue::String(new), SValue::Number(cap)] => SValue::String(telescope_core::names::fit_to_cap(old, new, *cap as usize).into()),
            _ => SValue::String("".into()),
        });
    }

    fn wire(self: &Rc<Self>, files: &[(String, String)], cards: &[CardPlace], headers: &[HeaderPlace]) {
        let ui = &self.ui;
        let state = self.state.clone();
        let _ = ui.set_global_callback("Folds", "is-folded", move |args| {
            let key = match args.first() { Some(SValue::String(s)) => s.to_string(), _ => String::new() };
            SValue::Bool(state.borrow().collapse.is_folded(&key))
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_global_callback("Folds", "toggle", move |args| {
            let Some(w) = weak.upgrade() else { return SValue::Void };
            if let Some(SValue::String(key)) = args.first() {
                if w.state.borrow_mut().collapse.toggle(key) {
                    w.fold_version.set(w.fold_version.get() + 1);
                    let _ = w.ui.set_global_property("Folds", "version", SValue::Number(w.fold_version.get() as f64));
                    w.schedule_save();
                }
            }
            SValue::Void
        });
        for (place, piece) in cards.iter().map(|c| ("card", &c.piece)).chain(headers.iter().map(|h| ("header", &h.piece))) {
            let Some(r) = self.pieces.iter().find(|r| r.id == piece.id) else { continue };
            for cb in &piece.iface.callbacks {
                let (plugin, callback, weak) = (r.plugin.clone(), cb.clone(), Rc::downgrade(self));
                let ui_name = place.to_string();
                let _ = ui.set_callback(&format!("{}-{cb}", piece.id), move |args| {
                    if let Some(w) = weak.upgrade() {
                        let event = UiEvent { ui: ui_name.clone(), callback: callback.clone(), args: args.iter().map(to_json).collect() };
                        w.state.borrow_mut().ui_event(&plugin, event);
                        w.process();
                    }
                    SValue::Void
                });
            }
        }
        let _ = files;
        self.wire_shortcut_menu();
        self.wire_text_cap();
        let weak = Rc::downgrade(self);
        let _ = ui.set_global_callback("Reveal", "show", move |args| {
            if let (Some(w), [SValue::Number(x), SValue::Number(y), _, SValue::Number(h)]) = (weak.upgrade(), args) {
                w.reveal(*x as f32, *y as f32, *h as f32);
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("start-clicked", move |_| {
            if let Some(w) = weak.upgrade() {
                w.toggle_stream();
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("tile-focus", move |args| {
            if let (Some(w), Some(SValue::String(id))) = (weak.upgrade(), args.first()) {
                let id = id.to_string();
                w.with_flow(move |flow, host| flow.focus_stream(host, &id));
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("add-picked", move |args| {
            if let (Some(w), Some(SValue::String(id))) = (weak.upgrade(), args.first()) {
                w.with_flow(|flow, host| flow.add_stream(host, id));
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("tile-stop", move |args| {
            if let (Some(w), Some(SValue::String(id))) = (weak.upgrade(), args.first()) {
                let id = id.to_string();
                w.with_flow(move |flow, host| flow.stop_stream(host, Some(&id)));
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("banner-action", move |args| {
            if let (Some(w), Some(SValue::String(key)), Some(SValue::Number(i))) = (weak.upgrade(), args.first(), args.get(1)) {
                let pressed = w.state.borrow_mut().banners.press(key, *i as usize);
                w.banner_pressed(key, pressed);
                w.process();
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("banner-dismissed", move |args| {
            if let (Some(w), Some(SValue::String(key))) = (weak.upgrade(), args.first()) {
                let pressed = w.state.borrow_mut().banners.dismiss(key);
                w.banner_pressed(key, pressed);
                w.process();
            }
            SValue::Void
        });
        crate::keys::watch_scan_codes(ui);
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("key", move |args| {
            let Some(w) = weak.upgrade() else { return SValue::Bool(false) };
            let (released, typing) = (args.get(1) == Some(&SValue::Bool(true)), args.get(2) == Some(&SValue::Bool(true)));
            let input = crate::keys::key_input(&args[0], released, typing, w.modal());
            SValue::Bool(w.key(&input))
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("settings-clicked", move |_| {
            if let Some(w) = weak.upgrade() {
                w.state.borrow_mut().effects.push(Effect::OpenSettings(String::new()));
                w.process();
            }
            SValue::Void
        });
        let _ = ui.set_property("window-title", SValue::String(if telescope_core::config::dev_profile().is_some() { tr("Telescope (dev)") } else { "Telescope".to_string() }.into()));
        self.show_idle();
    }

    /// Colour grading's compare: the shown stream's picture before its colour step left of the line.
    pub fn with_colour_split(&self, frame: telescope_core::Frame) -> telescope_core::Frame {
        let st = self.state.borrow();
        let (Some(position), Some(id)) = (st.colour.split(), st.streams.source_id.as_deref()) else { return frame };
        let before = st.colour_before.lock().unwrap().get(id).cloned();
        before.map_or(frame.clone(), |b| telescope_core::colour::with_split(&b, &frame, position))
    }

    /// Host::copy_text: onto the clipboard, through the window's hidden TextInput.
    /// Opens url, or copies what a copy_url holds (a banner's Copy command).
    pub fn follow_url(&self, url: &str) {
        match url.strip_prefix(crate::banners::COPY_SCHEME) {
            Some(text) => self.copy_text(text),
            None => open_url(url),
        }
    }

    pub(crate) fn copy_text(&self, text: &str) {
        let _ = self.ui.invoke("copy-text", &[SValue::String(text.into())]);
    }

    fn number(&self, name: &str) -> f32 {
        match self.ui.get_property(name) { Ok(SValue::Number(n)) => n as f32, _ => 0.0 }
    }

    /// Scrolls the column a control that just got keyboard focus is in, so it's in view (ensureWidgetVisible).
    fn reveal(&self, x: f32, y: f32, h: f32) {
        let (top, height) = (self.number("view-top"), self.number("view-height"));
        let col = (0..3).rev().find(|c| self.number(&format!("c{c}w")) > 0.0 && x - 16.0 >= self.number(&format!("c{c}x"))).unwrap_or(0);
        let name = format!("scroll{col}");
        let scroll = self.number(&name);
        let margin = 10.0;
        let to = if y < top + margin { scroll - (top + margin - y) } else if y + h > top + height - margin { scroll + (y + h - (top + height - margin)) } else { return };
        // Near the top it goes to the top: the card's own margin is part of what's brought into view
        let to = if to < margin { 0.0 } else { to };
        let _ = self.ui.set_property(&name, SValue::Number(f64::from(to)));
    }

    fn banner_pressed(self: &Rc<Self>, key: &str, pressed: crate::banners::Pressed) {
        use crate::banners::Pressed;
        use telescope_core::ui::IssueDoes;
        match pressed {
            Pressed::Action(IssueDoes::OpenSettings { tab }) => {
                if key == "plugins_off" {
                    let s = self.state.borrow_mut();
                    let _ = s.file.update(crate::switches::notice_off);
                }
                self.state.borrow_mut().effects.push(Effect::OpenSettings(tab));
            }
            Pressed::Action(IssueDoes::OpenUrl { url }) => self.follow_url(&url),
            Pressed::Action(IssueDoes::StartAgain { source_id, before }) => {
                // The plugin that showed it changes something first (Connect via back to Automatic)
                self.start_source(source_id, before.map(|action| (key.to_string(), action)));
            }
            Pressed::Action(IssueDoes::Plugin { id }) => self.tell_issue(key, &id),
            Pressed::Action(IssueDoes::CameraOnAgain { source_id }) => {
                // The switch is the panels' stream's
                if let Some(id) = source_id.as_deref() {
                    self.with_flow(|flow, host| flow.focus_stream(host, id));
                }
                self.state.borrow_mut().effects.push(Effect::CameraOn { on: true, auto: false });
            }
            Pressed::Dismissed => {
                if key == "plugins_off" {
                    let s = self.state.borrow_mut();
                    let _ = s.file.update(crate::switches::notice_off);
                } else {
                    self.tell_issue(key, "dismiss");
                }
            }
            Pressed::Nothing => {}
        }
    }

    /// A banner's plugin button: every plugin hears it and the one that showed it knows its key.
    pub(crate) fn tell_issue(&self, key: &str, id: &str) {
        let (key, id) = (key.to_string(), id.to_string());
        self.state.borrow_mut().each(|p, host| p.on_issue_action(host, &key, &id));
    }

    /// Each piece's starting properties from its UiSpec.
    fn first_props(&self) {
        let mut s = self.state.borrow_mut();
        let mut start = Vec::new();
        for slot in &s.slots {
            if let Some(card) = &slot.card {
                start.push(Effect::Props { plugin: slot.info.name.clone(), ui: "card".into(), props: card.ui.props.clone() });
            }
            if let Some(h) = &slot.header {
                start.push(Effect::Props { plugin: slot.info.name.clone(), ui: "header".into(), props: h.props.clone() });
            }
        }
        s.effects.splice(0..0, start);
    }

    /// Delivers queued events and carries out what plugins asked for, until nothing more comes.
    pub fn process(self: &Rc<Self>) {
        self.sync_camera(); // the events below may be about it, and plugins read it back
        for _ in 0..50 {
            self.state.borrow_mut().deliver_events();
            let queued = self.state.borrow().effects.len();
            if queued == 0 {
                break;
            }
            // Off the front, so a nested process (the stream flow's) carries out older effects before the ones its hooks add
            for _ in 0..queued {
                let next = {
                    let mut state = self.state.borrow_mut();
                    if state.effects.is_empty() {
                        break;
                    }
                    state.effects.remove(0)
                };
                self.apply(next);
            }
        }
        self.sync_camera();
        self.show_add_button();
        self.show_banners();
        self.paint_pick();
        self.paint_layers();
    }

    /// A stream's slot got a frame (WakingSlot): paints the newest one now, and any that came after the wake was asked for with it.
    pub fn frame_ready(self: &Rc<Self>) {
        self.streams.frame_wake.store(false, std::sync::atomic::Ordering::SeqCst);
        self.paint_preview();
        self.paint_pick();
        self.paint_layers();
    }

    /// The preview's newest frame from the stream the panels show, painted at the size of the pop-out window when it's
    /// open, else of the card's stage when the card shows one.
    pub fn paint_preview(self: &Rc<Self>) {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == "preview" && r.ui == "card") else { return };
        let prop = |name: &str| self.ui.get_property(&format!("{}-{name}", r.id)).ok();
        let on = |name: &str| prop(name) == Some(SValue::Bool(true));
        self.follow_popout(on("popout"), match prop("popout-asks") { Some(SValue::Number(n)) => n as i32, _ => 0 });
        let popout = self.popout.borrow().as_ref().map(|p| p.clone_strong());
        if let Some(p) = &popout {
            for name in ["stage-cursor", "marker-shown", "marker-u", "marker-v", "lens-boxes", "boxes-opacity", "skeleton-lines"] {
                if let Some(value) = prop(name) {
                    if p.get_property(name).ok().as_ref() != Some(&value) {
                        let _ = p.set_property(name, value);
                    }
                }
            }
        }
        // A new stream's first frame is a new one whatever its number, even with no paint since the last stream stopped
        let stream_gen = match prop("stream-gen") { Some(SValue::Number(n)) => n as i32, _ => 0 };
        if self.painted_gen.replace(stream_gen) != stream_gen {
            self.painted.set(0);
        }
        // _clear: nothing to show, on the pop-out too, and the next stream's first frame is a new one whatever its number
        if !on("camera-on") || !self.is_streaming() {
            if let Some(p) = &popout {
                let _ = p.set_property("has-frame", SValue::Bool(false));
            }
            self.painted.set(0);
            return;
        }
        let card = on("visible-card") && on("showing");
        let window = self.ui.window();
        let seen = window.is_visible() && !window.is_minimized();
        if !crate::preview::wants_frames(popout.is_some(), card, seen) {
            return;
        }
        let Some((frame, number)) = self.preview_frame() else { return };
        let frame = self.with_colour_split(frame);
        self.state.borrow_mut().overlays.shown_frame(&frame);
        if let Some(p) = &popout {
            fit_popout(p, frame.width, frame.height);
        }
        // A pass asked for the preview to be drawn again
        let (passes, dirty) = {
            let st = self.state.borrow();
            (crate::preview_passes::PassWork::sorted(&st.passes), st.preview_dirty.swap(false, std::sync::atomic::Ordering::SeqCst))
        };
        if dirty {
            self.painted.set(0);
        }
        if number == self.painted.get() {
            return;
        }
        // Half the frames on the card unless Smooth is on; what it skips only changes what's painted here
        if !self.cadence.borrow_mut().due(number, popout.is_some() || on("smooth-on"), self.painted.get() == 0) {
            return;
        }
        let stage = match &popout {
            Some(p) => (p.window().size().width, p.window().size().height),
            None => {
                let scale = self.ui.window().scale_factor();
                let px = |name: &str| match prop(name) {
                    Some(SValue::Number(v)) => (v as f32 * scale).round().max(0.0) as u32,
                    _ => 0,
                };
                (px("stage-width"), px("stage-height"))
            }
        };
        let (w, h) = crate::preview::fit_within((frame.width, frame.height), stage);
        if !self.logged.replace(true) {
            log::info!("Preview: {}x{} {:?} frames shown at {w}x{h}", frame.width, frame.height, frame.format);
        }
        // Passes that tint the picture, or want a look at it, work on a CPU copy at the size it is painted at
        let tinted = if self.passes.borrow().needs_copy(&passes) {
            let scaler = || self.services.as_ref().map(|s| s.media.scaler());
            let mut work = self.passes.borrow_mut();
            work.copy(scaler, &frame, w, h).map(|copy| {
                work.analyse(&passes, &copy, &self.state.borrow().preview_dirty);
                let mut shown = copy;
                crate::preview_passes::PassWork::tint(&passes, &mut shown);
                shown
            })
        } else {
            None
        };
        let image = match tinted.filter(|f| passes.iter().any(|(_, p)| p.tints()) && f.data.len() == (w * h * 4) as usize) {
            Some(shown) => {
                let rgb: Vec<u8> = shown.data.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
                slint::Image::from_rgb8(slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&rgb, w, h))
            }
            None => {
                let mut painter = self.painter.borrow_mut();
                if painter.is_none() {
                    let Some(services) = &self.services else { return };
                    *painter = Some(Box::new(crate::preview::CpuPainter(services.media.scaler())));
                }
                let Some(image) = painter.as_mut().and_then(|p| p.paint(&frame, w, h)) else { return };
                image
            }
        };
        self.painted.set(number);

        let (target, id) = match &popout {
            Some(p) => (p, ""),
            None => (&self.ui, r.id.as_str()),
        };
        let name = |n: &str| if id.is_empty() { n.to_string() } else { format!("{id}-{n}") };
        let _ = target.set_property(&name("frame"), SValue::Image(image));
        let _ = target.set_property(&name("has-frame"), SValue::Bool(true));
        // What passes draw over the frame, as one picture the size it's shown at
        let mut work = self.passes.borrow_mut();
        let shown = crate::preview::shown_size((frame.width, frame.height), stage);
        if work.overlay_due(dirty, shown) {
            let overlay = work.overlay(&passes, shown.0, shown.1, self.ui.window().scale_factor());
            let _ = target.set_property(&name("has-overlay"), SValue::Bool(overlay.is_some()));
            if let Some(overlay) = overlay {
                let _ = target.set_property(&name("overlay"), SValue::Image(overlay));
            }
        }
    }

    /// The shapes plugins draw on the preview card's picture (add_preview_layer), at the picture's size now.
    pub fn paint_layers(&self) {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == "preview" && r.ui == "card") else { return };
        // On the pop-out while it's open: the card is hidden then
        let popout = self.popout.borrow().as_ref().map(|p| p.clone_strong());
        let target = popout.as_ref().unwrap_or(&self.ui);
        let name = |n: &str| if popout.is_some() { n.to_string() } else { format!("{}-{n}", r.id) };
        let number = |n: &str| match target.get_property(&name(n)) { Ok(SValue::Number(v)) => v as f32, _ => 0.0 };
        let warped = SValue::Bool(self.state.borrow().overlays.warping());
        if target.get_property(&name("warped")).ok().as_ref() != Some(&warped) {
            let _ = target.set_property(&name("warped"), warped);
        }
        let drawing = crate::overlays::layer_drawing(&self.state.borrow().layers, number("pic-width"), number("pic-height"));
        if *self.layers_drawn.borrow() == drawing {
            return;
        }
        let colour = |c: Option<[u8; 4]>| {
            let [r, g, b, a] = c.unwrap_or([0; 4]);
            SValue::Brush(slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b)))
        };
        let paths: Vec<SValue> = drawing.0.iter().map(|p| {
            SValue::Struct(Struct::from_iter([
                ("commands".to_string(), SValue::String(p.commands.as_str().into())),
                ("stroke".to_string(), colour(p.stroke)),
                ("width".to_string(), SValue::Number(if p.stroke.is_some() { p.width as f64 } else { 0.0 })),
                ("fill".to_string(), colour(p.fill)),
                ("evenodd".to_string(), SValue::Bool(p.evenodd)),
            ]))
        }).collect();
        let texts: Vec<SValue> = drawing.1.iter().map(|t| {
            SValue::Struct(Struct::from_iter([
                ("x".to_string(), SValue::Number(t.x as f64)),
                ("y".to_string(), SValue::Number(t.y as f64)),
                ("text".to_string(), SValue::String(t.text.as_str().into())),
                ("color".to_string(), colour(Some(t.color))),
                ("size".to_string(), SValue::Number(t.size as f64)),
            ]))
        }).collect();
        let _ = target.set_property(&name("layer-paths"), SValue::Model(ModelRc::new(VecModel::from(paths))));
        let _ = target.set_property(&name("layer-texts"), SValue::Model(ModelRc::new(VecModel::from(texts))));
        *self.layers_drawn.borrow_mut() = drawing;
    }

    /// The pick under way on the preview's picture (overlays.rs), drawn from the overlays as they are now, on the pop-out
    /// while it's open and the card otherwise.
    fn paint_pick(&self) {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == "preview" && r.ui == "card") else { return };
        let popout = self.popout.borrow().as_ref().map(|p| p.clone_strong());
        let target = popout.as_ref().unwrap_or(&self.ui);
        let name = |n: &str| if popout.is_some() { n.to_string() } else { format!("{}-{n}", r.id) };
        let number = |n: &str| match target.get_property(&name(n)) { Ok(SValue::Number(v)) => v as f32, _ => 0.0 };
        let st = self.state.borrow();
        let drawing = st.overlays.pick_drawing(&st.view);
        let shown = target.get_property(&name("picking")) == Ok(SValue::Bool(true));
        if drawing.is_none() && !shown {
            return;
        }
        let (dim, line, handles) = drawing.as_ref().map(|d| d.paths(number("pic-width"), number("pic-height"))).unwrap_or_default();
        let cursor = st.overlays.cursor.map_or("", |c| c.name());
        let picking = drawing.is_some();
        let hint = drawing.map(|d| d.hint).unwrap_or_default();
        let text = |t: &str| SValue::String(t.into());
        let props = [("picking", SValue::Bool(picking)), ("pick-dim", text(&dim)), ("pick-line", text(&line)),
            ("pick-handles", text(&handles)), ("pick-hint", text(&hint)), ("pick-cursor", text(cursor))];
        for (n, v) in props {
            if target.get_property(&name(n)).ok().as_ref() != Some(&v) {
                let _ = target.set_property(&name(n), v);
            }
        }
    }

    /// Opens or closes the pop-out window as the preview card asks, and brings it forward when Pop out is pressed again.
    fn follow_popout(self: &Rc<Self>, open: bool, asks: i32) {
        let shown = self.popout.borrow().is_some();
        if open && !shown {
            match crate::compile::compile("popout.slint", POPOUT, Vec::new(), "PreviewPopout").and_then(|d| d.create().map_err(|e| e.to_string())) {
                Ok(p) => {
                    p.window().set_size(slint::LogicalSize::new(640.0, 360.0));
                    p.window().on_close_requested(|| {
                        with_window(|w| {
                            w.call_ui("preview", "card", "popout-closed");
                        });
                        slint::CloseRequestResponse::HideWindow
                    });
                    for callback in ["view-pointer", "view-wheel", "pick-pointer", "pick-key"] {
                        let weak = Rc::downgrade(self);
                        let _ = p.set_callback(callback, move |args| {
                            if let Some(w) = weak.upgrade() {
                                let event = UiEvent { ui: "card".into(), callback: callback.into(), args: args.iter().map(to_json).collect() };
                                w.state.borrow_mut().ui_event("preview", event);
                                w.process();
                            }
                            SValue::Void
                        });
                    }
                    set_app_icon(&p);
                    if let Ok(accent) = self.ui.get_global_property("Theme", "accent-soft") {
                        let _ = p.set_property("marker-colour", accent);
                    }
                    if let Ok(font) = self.ui.get_global_property("Theme", "font") {
                        let _ = p.set_property("layer-font", font);
                    }
                    if let Ok(colour) = self.ui.get_global_property("Theme", "warn") {
                        let _ = p.set_property("held-out", colour);
                    }
                    if let Ok(colour) = self.ui.invoke_global("Theme", "plugin-hue", &[SValue::String("vision".into())]) {
                        let _ = p.set_property("bone", colour);
                    }
                    let _ = p.show();
                    crate::frame::dress(&p);
                    *self.popout.borrow_mut() = Some(p);
                    self.layers_drawn.take();
                    self.painted.set(0);
                }
                Err(e) => log::error!("The preview couldn't pop out: {e}"),
            }
            self.popout_asks.set(asks);
        } else if !open && shown {
            if let Some(p) = self.popout.borrow_mut().take() {
                let _ = p.hide();
            }
            self.layers_drawn.take();
            self.painted.set(0);
        } else if open && asks != self.popout_asks.get() {
            self.popout_asks.set(asks);
            if let Some(p) = self.popout.borrow().as_ref() {
                let _ = p.show();
                bring_forward(p);
            }
        }
    }

    /// Hides every card but plugin's, so it alone sits at the top of its column; false when the plugin has no card.
    pub fn only_card(&self, plugin: &str) -> bool {
        let _ = self.ui.set_property("card-folds", SValue::Bool(false));
        let mut found = false;
        for r in self.pieces.iter().filter(|r| r.ui == "card") {
            found |= r.plugin == plugin;
            if r.plugin != plugin {
                let _ = self.ui.set_property(&format!("{}-visible", r.id), SValue::Bool(false));
            }
        }
        found
    }

    /// Where plugin's card is in the window, in logical pixels: (x, y, width, height).
    pub fn card_rect(&self, plugin: &str) -> Option<(f32, f32, f32, f32)> {
        let r = self.pieces.iter().find(|r| r.plugin == plugin && r.ui == "card")?;
        let e = i_slint_backend_testing::ElementHandle::find_by_element_id(&self.ui, &format!("TelescopeWindow::{}", r.id)).next()?;
        let (pos, size) = (e.absolute_position(), e.size());
        Some((pos.x, pos.y, size.width, size.height))
    }

    /// Puts an event on the bus as if a plugin sent it, and lets the plugins hear it (scenes stand in for unported plugins).
    pub fn emit(self: &Rc<Self>, event: telescope_core::event::Event) {
        self.state.borrow_mut().bus.emit(event);
        self.process();
    }

    /// Runs a callback of a plugin's card or header widget as if it had been clicked (scene renders open dialogs this way).
    pub fn call_ui(self: &Rc<Self>, plugin: &str, ui: &str, callback: &str) -> bool {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == plugin && r.ui == ui) else { return false };
        let done = self.ui.invoke(&format!("{}-{callback}", r.id), &[]).is_ok();
        self.process();
        done
    }

    /// Draws the preview through this from now on, in place of the graphics card's painter or the CPU's.
    pub fn use_painter(&self, painter: Box<dyn crate::preview::Painter>) {
        *self.painter.borrow_mut() = Some(painter);
    }

    /// The preview's pop-out window, while it's open.
    pub fn popout_window(&self) -> Option<ComponentInstance> {
        self.popout.borrow().as_ref().map(|p| p.clone_strong())
    }

    /// HostUi::open_window: builds the plugin's window and shows it, or shows it again when it's open.
    fn open_plugin_window(self: &Rc<Self>, plugin: &str, id: &str, spec: &telescope_core::ui::WindowSpec) {
        if let Some((_, _, open)) = self.plugin_windows.borrow().iter().find(|(p, i, _)| p == plugin && i == id) {
            let _ = open.show();
            crate::parent::attach(open, &self.ui);
            crate::parent::raise(open, &self.ui);
            return;
        }
        let (file, text) = source_of(plugin, id, &spec.ui);
        let component = &spec.ui.component;
        // Its file goes through the loader as a card's does, so its texts are translated
        let doc = format!("export {{ {component} }} from \"{file}\";\nexport {{ KeyFocus, WindowBox }} from \"@ui-kit/ui-kit.slint\";\n");
        let files = match &spec.ui.source {
            SlintSource::Text(_) => vec![(file.clone(), text.clone())],
            SlintSource::File(_) => Vec::new(),
        };
        // Under the main window, as Python gives a plugin's window the host as its parent
        let built = crate::compile::compile(&format!("{plugin}-{id}-window.slint"), &doc, files, component)
            .and_then(|d| crate::parent::make(&self.ui, || d.create()).map_err(|e| e.to_string()));
        let ui = match built {
            Ok(ui) => ui,
            Err(e) => {
                log::error!("{plugin}'s window {id} didn't build: {e}");
                return;
            }
        };
        ui.window().set_size(slint::LogicalSize::new(spec.size[0], spec.size[1]));
        for cb in scan(&text, component).callbacks {
            let (plugin, id, weak) = (plugin.to_string(), id.to_string(), Rc::downgrade(self));
            let callback = cb.clone();
            let _ = ui.set_callback(&cb, move |args| {
                if let Some(w) = weak.upgrade() {
                    let event = UiEvent { ui: id.clone(), callback: callback.clone(), args: args.iter().map(to_json).collect() };
                    w.state.borrow_mut().ui_event(&plugin, event);
                    w.process();
                }
                SValue::Void
            });
        }
        let (p, i, weak) = (plugin.to_string(), id.to_string(), Rc::downgrade(self));
        ui.window().on_close_requested(move || {
            let (p, i, weak) = (p.clone(), i.clone(), weak.clone());
            // After the close request returns, since dropping the window inside its own handler isn't safe
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(w) = weak.upgrade() {
                    w.close_plugin_window(&p, &i);
                }
            });
            slint::CloseRequestResponse::HideWindow
        });
        crate::keys::watch_plugin_keys(&ui, Rc::downgrade(self));
        Self::set_window_props(&ui, plugin, id, &spec.ui.props);
        let _ = ui.show();
        crate::frame::dress(&ui);
        crate::parent::attach(&ui, &self.ui);
        let scale = ui.window().scale_factor();
        crate::screen::keep_inside(&ui, ((320.0 * scale) as u32, (200.0 * scale) as u32));
        self.plugin_windows.borrow_mut().push((plugin.to_string(), id.to_string(), ui));
    }

    /// A plugin's own window while it's open.
    pub fn plugin_window(&self, plugin: &str, id: &str) -> Option<ComponentInstance> {
        self.plugin_windows.borrow().iter().find(|(p, i, _)| p == plugin && i == id).map(|(_, _, w)| w.clone_strong())
    }

    /// HostUi::close_window, or the person closed it: gone, and the plugin hears "closed".
    fn close_plugin_window(self: &Rc<Self>, plugin: &str, id: &str) {
        let gone = {
            let mut windows = self.plugin_windows.borrow_mut();
            windows.iter().position(|(p, i, _)| p == plugin && i == id).map(|at| windows.remove(at))
        };
        let Some((_, _, ui)) = gone else { return };
        let _ = ui.hide();
        slint::Timer::single_shot(Duration::ZERO, move || drop(ui)); // a key the plugin answered by closing it is still on its way out of it
        self.state.borrow_mut().ui_event(plugin, UiEvent { ui: id.into(), callback: "closed".into(), args: Vec::new() });
        self.process();
    }

    fn set_window_props(ui: &ComponentInstance, plugin: &str, id: &str, props: &Value) {
        for (k, v) in props.as_object().into_iter().flatten() {
            if let Err(e) = ui.set_property(k, to_property(ui, k, v)) {
                log::warn!("{plugin} set {k} on its window {id}: {e:?}");
            }
        }
    }

    fn set_props(&self, plugin: &str, ui: &str, props: &Value) {
        if let Some((_, _, window)) = self.plugin_windows.borrow().iter().find(|(p, i, _)| p == plugin && i == ui) {
            return Self::set_window_props(window, plugin, ui, props);
        }
        let Some(r) = self.pieces.iter().find(|r| r.plugin == plugin && r.ui == ui) else {
            // Not a card or a header: one of its settings sections, if the settings window has been built
            if let Some(settings) = crate::settings::current() {
                settings.set_props(plugin, ui, props);
            }
            return;
        };
        for (k, v) in props.as_object().into_iter().flatten() {
            let name = format!("{}-{k}", r.id);
            if let Err(e) = self.ui.set_property(&name, to_property(&self.ui, &name, v)) {
                log::warn!("{plugin} set {k} on its {ui}: {e:?}");
            }
        }
    }

    /// Host::set_ui_image: the picture goes onto the card's or section's image property.
    fn set_image(&self, plugin: &str, ui: &str, prop: &str, picture: &Frame) {
        let Some(image) = image_of(picture) else {
            log::warn!("{plugin} gave its {ui} a {:?} picture, which can't be shown", picture.format);
            return;
        };
        if let Some((_, _, window)) = self.plugin_windows.borrow().iter().find(|(p, i, _)| p == plugin && i == ui) {
            if let Err(e) = window.set_property(prop, SValue::Image(image)) {
                log::warn!("{plugin} set {prop} on its window {ui}: {e:?}");
            }
            return;
        }
        match self.pieces.iter().find(|r| r.plugin == plugin && r.ui == ui) {
            Some(r) => {
                if let Err(e) = self.ui.set_property(&format!("{}-{prop}", r.id), SValue::Image(image)) {
                    log::warn!("{plugin} set {prop} on its {ui}: {e:?}");
                }
            }
            None => {
                if let Some(settings) = crate::settings::current() {
                    settings.set_image(plugin, ui, prop, image);
                }
            }
        }
    }

    fn apply(self: &Rc<Self>, effect: Effect) {
        match effect {
            Effect::Props { plugin, ui, props } => self.set_props(&plugin, &ui, &props),
            Effect::Image { plugin, ui, prop, picture } => self.set_image(&plugin, &ui, &prop, &picture.0),
            Effect::AddSource(source) => self.add_plugin_source(source.0),
            Effect::RemoveSource(id) => self.remove_plugin_source(&id),
            Effect::StreamSource(id) => self.stream_plugin_source(&id),
            Effect::OpenWindow { plugin, id, window } => self.open_plugin_window(&plugin, &id, &window),
            Effect::CloseWindow { plugin, id } => self.close_plugin_window(&plugin, &id),
            Effect::RefreshActions(plugin) => {
                let mut state = self.state.borrow_mut();
                if let Some(i) = state.index(&plugin) {
                    if let Some(actions) = state.call(i, |p, _| p.actions()) {
                        state.slots[i].actions = actions;
                    }
                }
            }
            Effect::SaveSoon => self.schedule_save(),
            Effect::Notify { title, body, urgent } => {
                if let Some(n) = self.services.as_ref().and_then(|s| s.notifier.clone()) {
                    n.notify(&title, &body, urgent);
                }
            }
            Effect::Start { interactive } => {
                if !self.is_streaming() && !self.streams.flow.borrow().sessions.preparing {
                    self.with_flow(|flow, host| flow.start(host, interactive, None));
                }
            }
            Effect::StartAgain(id) => self.start_source(id, None),
            Effect::Stop(id) => self.with_flow(move |flow, host| flow.stop_stream(host, id.as_deref())),
            Effect::StopAll => self.stop_all_streams(),
            Effect::Reconnect(id) => self.with_flow(move |flow, host| flow.reconnect_stream(host, id.as_deref())),
            Effect::UpdateOutput { change, source_id } => self.update_output(&change, source_id.as_deref()),
            Effect::MakeMain(id) => self.with_flow(|flow, host| {
                flow.make_main(host, &id);
            }),
            Effect::PickSource(id) => {
                let handled = Cell::new(false);
                self.with_flow(|flow, host| handled.set(flow.pick_source(host, id.as_deref())));
                if !handled.get() {
                    let prev = self.state.borrow().selected.clone();
                    self.state.borrow_mut().switch_device(prev.as_deref(), id.as_deref());
                    self.refresh_frame_steps(); // set_config has no host, so its frame step is asked for again
                }
            }
            Effect::FinishSwitch { new, start } => {
                self.state.borrow_mut().finish_switch(new.as_deref(), start);
                self.refresh_frame_steps();
            }
            Effect::FrameStep(_) => self.refresh_frame_steps(),
            Effect::CameraOn { on, auto } => self.set_camera_on(on, auto),
            Effect::MicChanged => self.mic_changed(),
            Effect::SourceSelected => self.source_selected(),
            Effect::ResolutionPending { width, height } => self.resolution_pending(width, height),
            Effect::SettleStream => self.settle_stream(),
            Effect::Schedule { plugin, token, ms } => {
                let timer = slint::Timer::default();
                let (weak, key) = (Rc::downgrade(self), (plugin.clone(), token.clone()));
                timer.start(slint::TimerMode::SingleShot, Duration::from_millis(ms), move || {
                    let Some(w) = weak.upgrade() else { return };
                    w.timers.borrow_mut().remove(&key);
                    let event = Event::TimerFired { owner: key.0.clone(), token: key.1.clone() };
                    w.state.borrow_mut().deliver_to(&key.0, &event);
                    w.process();
                });
                self.timers.borrow_mut().insert((plugin, token), timer);
            }
            Effect::CancelSchedule { plugin, token } => {
                self.timers.borrow_mut().remove(&(plugin, token));
            }
            Effect::PhoneControl { source_id, body } => self.phone_control(source_id.as_deref(), &body),
            Effect::RequestPhoneState { source_id } => self.request_phone_state(source_id.as_deref()),
            Effect::Packets(makers) => self.change_packets(makers),
            Effect::Quit => self.quit("A plugin quit the app"),
            Effect::Restart => {
                self.restart_app();
            }
            Effect::Tray(_) => self.refresh_tray_menu(),
            Effect::ToggleWindow => self.toggle_window(),
            Effect::KeepInTray(_) => {}
            Effect::OpenSettings(tab) => crate::settings::open(self, &tab),
            Effect::OpenUrl(url) => self.follow_url(&url),
            Effect::CopyText(text) => self.copy_text(&text),
            Effect::PluginHues(on) => {
                let _ = self.ui.set_global_property("Theme", "own-hues", SValue::Bool(on));
                crate::settings::set_hues(on);
            }
            Effect::RestartCanvas { plugin, request, shows_command } => self.restart_canvas(&plugin, request, shows_command),
            Effect::RemoveExtras { plugin, request } => self.remove_extras(&plugin, request),
            Effect::TakeStill { plugin, request, source, options, folder } => self.take_still(&plugin, request, &source, &options, &folder),
            Effect::PickFile { plugin, request, dialog } => {
                std::thread::spawn(move || {
                    let (ok, message, data) = crate::file_dialog::ask(&dialog);
                    let _ = slint::invoke_from_event_loop(move || with_window(|w| {
                        w.state.borrow_mut().deliver_to(&plugin, &Event::RequestDone { request, ok, message, data });
                        w.process();
                    }));
                });
            }
            Effect::HandOver { source_id, route, url } => self.hand_over(&source_id, route, url),
            Effect::ShortcutMenu => {
                let on = self.state.borrow().shortcut_menu.is_some();
                let version = match self.ui.get_global_property("Shortcuts", "version") { Ok(SValue::Number(n)) => n, _ => 0.0 };
                let _ = self.ui.set_global_property("Shortcuts", "available", SValue::Bool(on));
                let _ = self.ui.set_global_property("Shortcuts", "version", SValue::Number(version + 1.0));
            }
        }
    }

    pub fn schedule_save(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.save_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(500), move || {
            if let Some(w) = weak.upgrade() {
                if !w.state.borrow_mut().save_now() {
                    w.schedule_save();
                }
                w.process();
            }
        });
    }

    pub fn show_banners(&self) {
        let s = self.state.borrow();
        let rows: Vec<SValue> = s.banners.all().iter().map(|(key, issue)| {
            let actions: Vec<SValue> = issue.actions.iter().map(|a| SValue::String(a.label.as_str().into())).collect();
            SValue::Struct(Struct::from_iter([
                ("key".to_string(), SValue::String(key.as_str().into())),
                ("title".to_string(), SValue::String(issue.title.as_str().into())),
                ("text".to_string(), SValue::String(issue.text.as_str().into())),
                ("details".to_string(), SValue::String(issue.details.as_str().into())),
                ("kind".to_string(), SValue::String(issue.kind.as_str().into())),
                ("actions".to_string(), SValue::Model(ModelRc::new(VecModel::from(actions)))),
            ]))
        }).collect();
        let _ = self.ui.set_property("banners", SValue::Model(ModelRc::new(VecModel::from(rows))));
    }

    pub(crate) fn set(&self, name: &str, text: impl Into<SharedString>) {
        let _ = self.ui.set_property(name, SValue::String(text.into()));
    }

    /// _show_focus_stopped and the start button as it is while idle.
    fn show_idle(&self) {
        self.show_idle_button();
        self.set("status", tr("Not streaming"));
        self.set("status-kind", "status_dim");
        self.set("fps", "-");
        self.set("net", "-");
    }

    /// _set_start_button(streaming=False).
    pub(crate) fn show_idle_button(&self) {
        let camera_off = self.state.borrow().streams.camera_off;
        self.set("start-text", if camera_off { tr("Start mic only") } else { tr("Start streaming") });
        self.set("start-short", if camera_off { tr("Start mic only") } else { tr("Start") });
        let _ = self.ui.set_property("streaming", SValue::Bool(false));
    }

    pub(crate) fn show_streaming(&self) {
        self.set("start-text", tr("Stop streaming"));
        self.set("start-short", tr("Stop"));
        let _ = self.ui.set_property("streaming", SValue::Bool(true));
    }

    /// restart_app: the launcher starts a supervised copy again from its exit code; any other starts a new copy of itself,
    /// which waits for this one to let go.
    pub(crate) fn restart_app(&self) -> bool {
        if telescope_core::dev_profile::supervised() {
            EXIT_CODE.store(restart_exit_code(self.is_streaming()), Ordering::SeqCst);
            self.quit("Restarting");
            return true;
        }
        let again = telescope_core::system::start_command().and_then(|mut argv| {
            argv.push("--restarted".to_string());
            (app_hooks().relaunch)(&argv)
        });
        match again {
            Ok(()) => {
                self.quit("Restarting");
                true
            }
            Err(e) => {
                log::error!("Couldn't start Telescope again: {e}");
                false
            }
        }
    }

    pub fn shut_down(self: &Rc<Self>) {
        self.stop_streams_for_quit();
        if self.save_timer.running() {
            self.save_timer.stop();
            self.state.borrow_mut().save_now();
        }
        self.state.borrow_mut().each(|p, host| p.shutdown(host));
        let vision = self.state.borrow().vision.clone();
        if let Some(vision) = vision {
            vision.shutdown();
        }
    }
}

fn open_url(url: &str) {
    telescope_core::launch::open_url(url);
}

pub use crate::tray::{raise, restart};

/// Lets go of the window build() registered, once it's shut down (the scene renders build one after another).
pub fn forget() {
    WINDOW.with(|w| *w.borrow_mut() = None);
}

/// The window, until it's closed.
/// What only the program around the window knows; the binary hands them over before the window opens (the plain defaults
/// are for tests and tools).
pub struct AppHooks {
    /// Which Telescope this is, as the UI says it, and its commit.
    pub build: fn() -> String,
    pub commit: fn() -> &'static str,
    /// Hands memory the allocator kept after frames were freed back to the system.
    pub release_memory: fn(),
    /// Starts argv on its own, outliving this process.
    pub relaunch: fn(&[String]) -> std::io::Result<()>,
}

static APP_HOOKS: OnceLock<AppHooks> = OnceLock::new();

pub fn use_app_hooks(hooks: AppHooks) {
    let _ = APP_HOOKS.set(hooks);
}

pub(crate) fn app_hooks() -> &'static AppHooks {
    APP_HOOKS.get_or_init(|| AppHooks {
        build: || telescope_core::version::version().to_string(),
        commit: || "",
        release_memory: || {},
        relaunch: |_| Err(std::io::ErrorKind::Unsupported.into()),
    })
}

/// What the process should exit with once run() is back, for the dev launcher.
static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

pub fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

fn restart_exit_code(streaming: bool) -> i32 {
    use telescope_core::dev_profile::{RESTART_EXIT, RESTART_STREAMING};
    if streaming { RESTART_STREAMING } else { RESTART_EXIT }
}

/// QApplication.platformName, for the diagnostics report.
pub(crate) fn windowing() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "wayland"
    } else {
        "x11"
    }
}

/// The window with every built-in plugin, on file.
pub fn build_app(file: ConfigFile, services: Arc<Services>, opts: Option<WindowOptions>) -> Result<Rc<Window>, String> {
    build(file, |bus| {
        let mut plugins = crate::plugins::builtins(Some(&services), bus);
        plugins.extend((services.plugins)());
        crate::plugins::in_order(plugins)
    }, Some(services.clone()), opts)
}

pub fn run(services: Arc<Services>, opts: WindowOptions) -> Result<(), String> {
    let gpu_painter = crate::preview::select_wgpu_backend();
    // Without the graphics card's backend the default one is chosen here, so Windows can make a side window owned by the main one
    #[cfg(windows)]
    if gpu_painter.is_none() {
        if let Err(e) = crate::parent::selector().select() {
            log::warn!("The window backend couldn't be chosen: {e}");
        }
    }
    // Wayland takes a window's icon from the desktop entry named like this, not from the window
    if let Some(id) = APP_ID.get() {
        if let Err(e) = slint::set_xdg_app_id(id.as_str()) {
            log::info!("Couldn't set the window's app id: {e}");
        }
    }
    let window = build_app(ConfigFile::default(), services.clone(), Some(opts.clone()))?;
    *window.painter.borrow_mut() = gpu_painter;
    if let Some(t0) = std::env::var("TELESCOPE_T0").ok().and_then(|v| v.parse::<u64>().ok()) {
        let mut done = false;
        let _ = window.ui.window().set_rendering_notifier(move |state, _| {
            if !done && matches!(state, slint::RenderingState::AfterRendering) {
                done = true;
                println!("first-paint-ms {:.1}", (telescope_core::time::monotonic_ns() - t0) as f64 / 1e6);
            }
        });
    }
    // A new frame wakes the window (frame_ready), so this only covers what has no frame to say it changed: focus, a picture that wants
    // drawing again with the stream stalled, and the tiles' tiny pictures, so 10 fps costs next to nothing
    let weak = Rc::downgrade(&window);
    window.preview_timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        if let Some(w) = weak.upgrade() {
            w.paint_preview();
            w.paint_pick();
            w.paint_layers();
            w.watch_focus();
            w.update_thumbnails();
        }
    });
    if opts.autostart {
        let weak = Rc::downgrade(&window);
        slint::Timer::single_shot(Duration::from_millis(100), move || {
            if let Some(w) = weak.upgrade() {
                w.toggle_stream();
            }
        });
    }
    if let Some(secs) = opts.quit_after {
        slint::Timer::single_shot(Duration::from_secs(secs), || {
            let _ = slint::quit_event_loop();
        });
    }
    if let Some(out) = opts.snapshot.clone() {
        let weak = Rc::downgrade(&window);
        window.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
        slint::Timer::single_shot(Duration::from_millis(if opts.autostart { 8000 } else { 1500 }), move || {
            if let Some(w) = weak.upgrade() {
                crate::snapshot::save(w.ui.window(), &out);
            }
            let _ = slint::quit_event_loop();
        });
    }
    if opts.minimized {
        window.start_hidden();
    } else {
        window.show_window();
    }
    if let Some(hook) = &opts.on_shown {
        (hook.0)();
    }
    let weak = Rc::downgrade(&window);
    window.ui.window().on_close_requested(move || weak.upgrade().map_or(slint::CloseRequestResponse::HideWindow, |w| w.close_requested()));
    window.refresh_tray_menu();
    let tray_timer = slint::Timer::default();
    if window.has_tray() {
        let weak = Rc::downgrade(&window);
        tray_timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            if let Some(w) = weak.upgrade() {
                w.poll_tray();
            }
        });
    }
    if crate::tray::take_raise() {
        window.show_window();
    }
    window.process();
    slint::run_event_loop_until_quit().map_err(|e| e.to_string())?;
    drop(tray_timer);
    let _ = window.ui.hide();
    window.shut_down();
    WINDOW.with(|w| *w.borrow_mut() = None);
    Ok(())
}

#[cfg(test)]
mod popout_tests {
    #[test]
    fn a_restart_tells_the_dev_launcher_whether_to_stream_again() {
        assert_eq!((super::restart_exit_code(false), super::restart_exit_code(true)), (75, 76));
    }

    #[test]
    fn the_pop_out_compiles() {
        i_slint_backend_testing::init_no_event_loop();
        crate::compile::compile("popout.slint", super::POPOUT, Vec::new(), "PreviewPopout").unwrap();
    }

    #[test]
    fn the_app_icon_is_a_picture_a_window_can_use() {
        i_slint_backend_testing::init_no_event_loop();
        let icon = super::app_icon().expect("the icon");
        assert_eq!((icon.size().width, icon.size().height), (256, 256));
        assert!(icon.to_rgba8().is_some(), "an SVG would come back empty, and winit shows nothing for it");
    }

    #[test]
    fn popout_set_frame_and_resize_guards() {
        use slint::ComponentHandle;
        i_slint_backend_testing::init_no_event_loop();
        let p = crate::compile::compile("popout.slint", super::POPOUT, Vec::new(), "PreviewPopout").unwrap().create().unwrap();
        let size = |w, h| p.window().set_size(slint::PhysicalSize::new(w, h));
        let now = || (p.window().size().width, p.window().size().height);
        size(640, 400);
        super::fit_popout(&p, 160, 90);
        assert_eq!(now(), (640, 360), "by its width, the stream's shape");
        size(640, 362);
        super::fit_popout(&p, 160, 90);
        assert_eq!(now(), (640, 362), "off by less than 4 px stays");
        size(640, 365);
        super::fit_popout(&p, 160, 90);
        assert_eq!(now(), (640, 360));
        size(500, 300);
        super::fit_popout(&p, 0, 0);
        assert_eq!(now(), (500, 300), "no frame to take the shape of");
    }
}
