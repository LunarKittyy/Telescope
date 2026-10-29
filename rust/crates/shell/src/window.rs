//! The window: builds the document from the registered plugins (doc.rs), wires its callbacks back to them and carries
//! out what they ask for (shell::Effect). Streams go through the stream flow (streams.rs).

use crate::doc::{self, CardPlace, HeaderPlace, Piece};
use crate::layout::Region;
use crate::shell::{Effect, State};
use crate::source::{scan, to_json, to_slint};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use slint_interpreter::{ComponentInstance, Struct, Value as SValue};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use telescope_core::config::ConfigFile;
use telescope_core::event::Event;
use telescope_core::i18n::tr;
use telescope_core::link::{RoutePrefs, StreamRequest};
use telescope_core::plugin::Plugin;
use telescope_core::services::Services;
use telescope_core::source::Phones;
use telescope_core::ui::{SlintSource, UiEvent, UiSpec};
use telescope_core::BusSender;

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
    pub(crate) streams: crate::streams::Streams,
    /// Plugins' pending host.schedule timers, by (plugin, token).
    timers: RefCell<std::collections::BTreeMap<(String, String), slint::Timer>>,
    /// Draws the preview's frames: on Slint's device once it says which, else on the CPU (preview.rs).
    painter: RefCell<Option<Box<dyn crate::preview::Painter>>>,
    preview_timer: slint::Timer,
    /// The number of the frame the preview shows, so a frame isn't painted twice.
    painted: Cell<u64>,
    /// The preview's pop-out window while it's open, and the card's popout-asks it last answered.
    popout: RefCell<Option<ComponentInstance>>,
    popout_asks: Cell<i32>,
}

/// _PopoutWindow: the picture alone on black, in a window of its own.
const POPOUT: &str = r#"
export component PreviewPopout inherits Window {
    title: @tr("Telescope - Video Preview");
    min-width: 320px;
    min-height: 180px;
    background: #000;
    in property <image> frame;
    in property <bool> has-frame;
    if has-frame: Image {
        width: parent.width;
        height: parent.height;
        source: frame;
        image-fit: contain;
        image-rendering: smooth;
    }
}
"#;

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

/// Where a UiSpec's text comes from and the file the document imports it as.
fn source_of(plugin: &str, ui: &str, spec: &UiSpec) -> (String, String) {
    match &spec.source {
        SlintSource::Text(text) => (format!("plugins/{plugin}/{ui}.slint"), text.clone()),
        SlintSource::File(path) => (path.clone(), std::fs::read_to_string(path).unwrap_or_default()),
    }
}

/// Registers plugins into a fresh state and builds their window. services is None for scene renders.
pub fn build(file: ConfigFile, plugins: impl FnOnce(&BusSender) -> Vec<Box<dyn Plugin>>, services: Option<Arc<Services>>,
             opts: Option<WindowOptions>) -> Result<Rc<Window>, String> {
    let mut state = State::new(file);
    state.link = services.as_ref().map(|s| s.link.clone());
    let plugins = plugins(&state.bus.sender());
    state.load_builtins(plugins);
    state.apply_saved_config();
    let phones = state.plugin_config("connection").map_or_else(
        || phones_from_file(&state.file).phones.len() as u32,
        |c| c.get("phones").and_then(Value::as_array).map_or(0, |p| p.len() as u32));
    state.bus.emit(Event::PhonesChanged { count: phones });

    let (mut cards, mut headers, mut files, mut pieces) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, slot) in state.slots.iter().enumerate() {
        let name = &slot.info.name;
        if let Some(card) = &slot.card {
            let (file, text) = source_of(name, "card", &card.ui);
            let id = format!("p{i}");
            let title = if slot.info.title.is_empty() { name.clone() } else { slot.info.title.clone() };
            cards.push(CardPlace {
                piece: Piece { id: id.clone(), file: file.clone(), component: card.ui.component.clone(), iface: scan(&text, &card.ui.component) },
                region: Region::from_name(&card.region),
                fold_key: format!("{name}/{title}"),
                category: slot.info.category.clone(),
            });
            files.push((file, text));
            pieces.push(PieceRef { plugin: name.clone(), ui: "card".into(), id });
        }
        if let Some(header) = &slot.header {
            let (file, text) = source_of(name, "header", header);
            let id = format!("h{i}");
            headers.push(HeaderPlace {
                piece: Piece { id: id.clone(), file: file.clone(), component: header.component.clone(), iface: scan(&text, &header.component) },
                right: slot.info.header_side == "right",
            });
            files.push((file, text));
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
        popout: RefCell::default(),
        popout_asks: Cell::new(0),
    });
    WINDOW.with(|w| *w.borrow_mut() = Some(window.clone()));
    // Events emitted from other threads get delivered on this one
    window.state.borrow().bus.set_waker(|| {
        let _ = slint::invoke_from_event_loop(|| with_window(|w| w.process()));
    });
    window.state.borrow().posts.set_waker(|| {
        let _ = slint::invoke_from_event_loop(|| with_window(|w| w.process()));
    });
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

    /// Host::copy_text: onto the clipboard, through the window's hidden TextInput.
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
        let _ = self.ui.set_property(&name, SValue::Number(to.max(0.0) as f64));
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
            Pressed::Action(IssueDoes::OpenUrl { url }) => open_url(&url),
            Pressed::Action(IssueDoes::StartAgain { source_id, before }) => {
                // The plugin that showed it changes something first (Connect via back to Automatic)
                if let Some(before) = before {
                    self.tell_issue(key, &before);
                }
                self.start_source(source_id);
            }
            Pressed::Action(IssueDoes::Plugin { id }) => self.tell_issue(key, &id),
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
    fn tell_issue(&self, key: &str, id: &str) {
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
        for _ in 0..50 {
            self.state.borrow_mut().deliver_events();
            let effects: Vec<Effect> = std::mem::take(&mut self.state.borrow_mut().effects);
            if effects.is_empty() {
                break;
            }
            for e in effects {
                self.apply(e);
            }
        }
        self.show_banners();
    }

    /// The preview's newest frame from the stream the panels show, painted at the size of the pop-out window when it's
    /// open, else of the card's stage when the card shows one.
    fn paint_preview(self: &Rc<Self>) {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == "preview" && r.ui == "card") else { return };
        let prop = |name: &str| self.ui.get_property(&format!("{}-{name}", r.id)).ok();
        let on = |name: &str| prop(name) == Some(SValue::Bool(true));
        self.follow_popout(on("popout"), match prop("popout-asks") { Some(SValue::Number(n)) => n as i32, _ => 0 });
        let popout = self.popout.borrow().as_ref().map(|p| p.clone_strong());
        let card = on("visible-card") && on("showing");
        if !(popout.is_some() || card) || !on("camera-on") || !self.is_streaming() {
            return;
        }
        let Some((frame, number)) = self.preview_frame() else { return };
        if let Some(p) = &popout {
            fit_popout(p, frame.width, frame.height);
        }
        if number == self.painted.get() {
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
        if self.painted.get() == 0 {
            log::info!("Preview: {}x{} {:?} frames shown at {w}x{h}", frame.width, frame.height, frame.format);
        }
        let mut painter = self.painter.borrow_mut();
        if painter.is_none() {
            let Some(services) = &self.services else { return };
            *painter = Some(Box::new(crate::preview::CpuPainter(services.media.scaler())));
        }
        let Some(image) = painter.as_mut().and_then(|p| p.paint(&frame, w, h)) else { return };
        drop(painter);
        self.painted.set(number);

        let (target, id) = match &popout {
            Some(p) => (p, ""),
            None => (&self.ui, r.id.as_str()),
        };
        let name = |n: &str| if id.is_empty() { n.to_string() } else { format!("{id}-{n}") };
        let _ = target.set_property(&name("frame"), SValue::Image(image));
        let _ = target.set_property(&name("has-frame"), SValue::Bool(true));
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
                    let _ = p.show();
                    *self.popout.borrow_mut() = Some(p);
                    self.painted.set(0);
                }
                Err(e) => log::error!("The preview couldn't pop out: {e}"),
            }
            self.popout_asks.set(asks);
        } else if !open && shown {
            if let Some(p) = self.popout.borrow_mut().take() {
                let _ = p.hide();
            }
            self.painted.set(0);
        } else if open && asks != self.popout_asks.get() {
            self.popout_asks.set(asks);
            if let Some(p) = self.popout.borrow().as_ref() {
                let _ = p.show();
            }
        }
    }

    /// Hides every card but plugin's, so it alone sits at the top of its column; false when the plugin has no card.
    pub fn only_card(&self, plugin: &str) -> bool {
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

    /// Runs a callback of a plugin's card or header widget as if it had been clicked (scene renders open dialogs this way).
    pub fn call_ui(self: &Rc<Self>, plugin: &str, ui: &str, callback: &str) -> bool {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == plugin && r.ui == ui) else { return false };
        let done = self.ui.invoke(&format!("{}-{callback}", r.id), &[]).is_ok();
        self.process();
        done
    }

    fn set_props(&self, plugin: &str, ui: &str, props: &Value) {
        let Some(r) = self.pieces.iter().find(|r| r.plugin == plugin && r.ui == ui) else { return };
        for (k, v) in props.as_object().into_iter().flatten() {
            let name = format!("{}-{k}", r.id);
            let like = self.ui.get_property(&name).unwrap_or(SValue::Void);
            if let Err(e) = self.ui.set_property(&name, to_slint(v, &like)) {
                log::warn!("{plugin} set {k} on its {ui}: {e:?}");
            }
        }
    }

    fn apply(self: &Rc<Self>, effect: Effect) {
        match effect {
            Effect::Props { plugin, ui, props } => self.set_props(&plugin, &ui, &props),
            Effect::SaveSoon => self.schedule_save(),
            Effect::Notify { title, body, urgent } => {
                if let Some(n) = self.services.as_ref().and_then(|s| s.notifier.clone()) {
                    n.notify(&title, &body, urgent);
                }
            }
            Effect::Start { interactive } => {
                if !self.is_streaming() {
                    self.with_flow(|flow, host| flow.start(host, interactive, None));
                }
            }
            Effect::StartAgain(id) => self.start_source(id),
            Effect::Stop(id) => self.with_flow(move |flow, host| flow.stop_stream(host, id.as_deref())),
            Effect::StopAll => self.stop_all_streams(),
            Effect::Reconnect => self.with_flow(|flow, host| flow.reconnect_stream(host, None)),
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
            Effect::FrameStep(_) => self.refresh_frame_steps(),
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
            Effect::Quit => self.quit("A plugin quit the app"),
            Effect::Tray(_) => self.refresh_tray_menu(),
            Effect::ToggleWindow => self.toggle_window(),
            Effect::KeepInTray(_) => {}
            Effect::OpenSettings(tab) => crate::settings::open(self, &tab),
            Effect::OpenUrl(url) => match url.strip_prefix(crate::banners::COPY_SCHEME) {
                Some(text) => self.copy_text(text),
                None => open_url(&url),
            },
            Effect::CopyText(text) => self.copy_text(&text),
            Effect::HandOver { source_id, route, url } => self.hand_over(&source_id, route, url),
            other => log::debug!("not handled yet: {other:?}"),
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
        self.set("fps", "—");
        self.set("net", "—");
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

    pub fn shut_down(self: &Rc<Self>) {
        self.stop_streams_for_quit();
        if self.save_timer.running() {
            self.save_timer.stop();
            self.state.borrow_mut().save_now();
        }
        self.state.borrow_mut().each(|p, host| p.shutdown(host));
    }
}

fn open_url(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

pub use crate::tray::raise;

/// Lets go of the window build() registered, once it's shut down (the scene renders build one after another).
pub fn forget() {
    WINDOW.with(|w| *w.borrow_mut() = None);
}

/// The window, until it's closed.
pub fn run(services: Arc<Services>, opts: WindowOptions) -> Result<(), String> {
    let gpu_painter = crate::preview::select_wgpu_backend();
    let window = build(ConfigFile::default(), |bus| {
        let mut plugins = crate::plugins::builtins(Some(&services), bus);
        plugins.extend((services.plugins)());
        crate::plugins::in_order(plugins)
    }, Some(services.clone()), Some(opts.clone()))?;
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
    // Frames are pulled at the display's pace rather than pushed by the stream, so a slow paint never holds it up
    let weak = Rc::downgrade(&window);
    window.preview_timer.start(slint::TimerMode::Repeated, Duration::from_millis(15), move || {
        if let Some(w) = weak.upgrade() {
            w.paint_preview();
            w.watch_focus();
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
    window.process();
    slint::run_event_loop_until_quit().map_err(|e| e.to_string())?;
    drop(tray_timer);
    let _ = window.ui.hide();
    window.shut_down();
    WINDOW.with(|w| *w.borrow_mut() = None);
    Ok(())
}
