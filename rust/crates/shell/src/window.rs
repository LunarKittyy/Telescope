//! The window: builds the document from the registered plugins (doc.rs), wires its callbacks back to them and carries
//! out what they ask for (shell::Effect). Streaming is still the prototype's single phone stream (flow.rs) until the
//! stream flow is ported.

use crate::doc::{self, CardPlace, HeaderPlace, Piece};
use crate::flow::{Stats, StreamEvents, StreamOptions, Streaming};
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
use telescope_core::link::{PhoneSession, RoutePrefs, StreamRequest};
use telescope_core::plugin::{Plugin, StreamInfo};
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
    services: Option<Arc<Services>>,
    opts: Option<WindowOptions>,
    stream: RefCell<Option<(Streaming, Arc<dyn PhoneSession>)>>,
}

thread_local! {
    static WINDOW: RefCell<Option<Rc<Window>>> = const { RefCell::new(None) };
}

/// The window on this thread, for callbacks that come back from other threads.
fn with_window(f: impl FnOnce(&Rc<Window>)) {
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
        stream: RefCell::new(None),
    });
    WINDOW.with(|w| *w.borrow_mut() = Some(window.clone()));
    // Events emitted from other threads get delivered on this one
    window.state.borrow().bus.set_waker(|| {
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
        let _ = ui.set_callback("start-clicked", move |_| {
            if let Some(w) = weak.upgrade() {
                w.toggle_stream();
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
            Pressed::Action(IssueDoes::StartAgain { source_id, .. }) => self.state.borrow_mut().effects.push(Effect::StartAgain(source_id)),
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
            Effect::Start { .. } | Effect::StartAgain(_) => {
                if self.stream.borrow().is_none() {
                    self.start_stream();
                }
            }
            Effect::Stop(_) | Effect::StopAll => self.stop_stream(),
            Effect::PhoneControl { body, .. } => {
                if let Some((_, session)) = self.stream.borrow().as_ref() {
                    let session = session.clone();
                    std::thread::spawn(move || {
                        let _ = session.send(&body);
                    });
                }
            }
            Effect::Quit => {
                let _ = slint::quit_event_loop();
            }
            Effect::OpenSettings(tab) => crate::settings::open(self, &tab),
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

    fn set(&self, name: &str, text: impl Into<SharedString>) {
        let _ = self.ui.set_property(name, SValue::String(text.into()));
    }

    /// _show_focus_stopped and the start button as it is while idle.
    fn show_idle(&self) {
        let camera_off = self.state.borrow().streams.camera_off;
        self.set("start-text", if camera_off { tr("Start mic only") } else { tr("Start streaming") });
        self.set("start-short", if camera_off { tr("Start mic only") } else { tr("Start") });
        let _ = self.ui.set_property("streaming", SValue::Bool(false));
        self.set("status", tr("Not streaming"));
        self.set("status-kind", "status_dim");
        self.set("fps", "—");
        self.set("net", "—");
    }

    fn show_streaming(&self) {
        self.set("start-text", tr("Stop streaming"));
        self.set("start-short", tr("Stop"));
        let _ = self.ui.set_property("streaming", SValue::Bool(true));
    }

    fn toggle_stream(self: &Rc<Self>) {
        if self.stream.borrow().is_some() {
            self.stop_stream();
        } else if !self.state.borrow().streams.starting {
            self.start_stream();
        }
        self.process();
    }

    fn start_stream(self: &Rc<Self>) {
        let (Some(services), Some(opts)) = (self.services.clone(), self.opts.clone()) else { return };
        let phones = phones_from_file(&self.state.borrow().file);
        let Some(phone) = phones.selected().cloned() else { return };
        self.state.borrow_mut().streams.starting = true;
        let _ = self.ui.set_property("start-enabled", SValue::Bool(false));
        self.set("status", tr("Connecting"));
        let weak = self.ui.as_weak();
        std::thread::spawn(move || {
            let result = services.link.resolve(&phone, opts.prefs).map_err(|e| format!("{e:?}")).and_then(|s| {
                let s: Arc<dyn PhoneSession> = s.into();
                s.start(opts.request).map(|_| s)
            });
            let services = services.clone();
            let _ = weak.upgrade_in_event_loop(move |_| with_window(|w| w.started(result, services)));
        });
    }

    fn started(self: &Rc<Self>, result: Result<Arc<dyn PhoneSession>, String>, services: Arc<Services>) {
        self.state.borrow_mut().streams.starting = false;
        let _ = self.ui.set_property("start-enabled", SValue::Bool(true));
        let session = match result {
            Ok(s) => s,
            Err(e) => {
                self.set("status", e);
                self.set("status-kind", "status_err");
                self.state.borrow_mut().bus.emit(Event::StreamStartFailed);
                self.process();
                return;
            }
        };
        let opts = self.opts.clone().expect("a stream needs options");
        let streaming = Streaming::start(session.clone(), services.media.clone(), services.outputs.clone(),
            StreamOptions { fps: opts.request.fps, slot: 0, hardware_decode: opts.hardware_decode },
            Arc::new(Events { ui: self.ui.as_weak() }));
        let info = StreamInfo { source_id: self.state.borrow().selected.clone().unwrap_or_default(), url: session.describe() };
        *self.stream.borrow_mut() = Some((streaming, session));
        {
            let mut s = self.state.borrow_mut();
            s.streams.streaming = true;
            s.streams.source_id = Some(info.source_id.clone());
            s.each(|p, host| p.on_stream_start(host, &info));
            s.bus.emit(Event::StreamStarted { url: info.url.clone() });
        }
        self.show_streaming();
        self.process();
    }

    fn stop_stream(self: &Rc<Self>) {
        let Some((streaming, session)) = self.stream.borrow_mut().take() else { return };
        std::thread::spawn(move || {
            streaming.stop();
            let _ = session.stop();
            session.release();
        });
        {
            let mut s = self.state.borrow_mut();
            s.streams.streaming = false;
            s.streams.phone_state = None;
            s.each(|p, host| p.on_stream_stop(host));
            s.bus.emit(Event::StreamStopped);
        }
        self.show_idle();
        self.process();
    }

    fn phone_state(self: &Rc<Self>, state: Value) {
        {
            let mut s = self.state.borrow_mut();
            s.streams.phone_state = Some(state.clone());
            s.bus.emit(Event::PhoneStateUpdated { state: state.clone() });
            s.each(|p, host| p.on_phone_state(host, &state));
        }
        self.process();
    }

    pub fn shut_down(self: &Rc<Self>) {
        self.stop_stream();
        if self.save_timer.running() {
            self.save_timer.stop();
            self.state.borrow_mut().save_now();
        }
        self.state.borrow_mut().each(|p, host| p.shutdown(host));
    }
}

fn open_url(url: &str) {
    if let Some(text) = url.strip_prefix(crate::banners::COPY_SCHEME) {
        // No clipboard in core yet: the command shows in the banner, selectable, and in the log
        log::info!("Copy: {text}");
        return;
    }
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

struct Events {
    ui: slint::Weak<ComponentInstance>,
}

impl StreamEvents for Events {
    fn preview(&self, _rgb: Vec<u8>, _width: u32, _height: u32) {}

    fn state(&self, state: Value) {
        let _ = self.ui.upgrade_in_event_loop(move |_| with_window(|w| w.phone_state(state)));
    }

    fn stats(&self, s: Stats) {
        let fps = format!("{:.0}", s.fps);
        let net = format!("{:.1} Mbps", s.mbps);
        let status = format!("{} {}x{}", tr("Live"), s.width, s.height);
        let _ = self.ui.upgrade_in_event_loop(move |_| with_window(|w| {
            w.set("fps", fps);
            w.set("net", net);
            w.set("status", status);
            w.set("status-kind", "status_ok");
        }));
    }

    fn ended(&self, why: String) {
        let _ = self.ui.upgrade_in_event_loop(move |_| with_window(|w| {
            w.stop_stream();
            if !why.is_empty() {
                w.set("status", why);
            }
        }));
    }
}

/// The window, until it's closed.
pub fn run(services: Arc<Services>, opts: WindowOptions) -> Result<(), String> {
    let window = build(ConfigFile::default(), |bus| {
        let mut plugins = crate::plugins::builtins(Some(&services), bus);
        plugins.extend((services.plugins)());
        plugins
    }, Some(services.clone()), Some(opts.clone()))?;
    if let Some(t0) = std::env::var("TELESCOPE_T0").ok().and_then(|v| v.parse::<u64>().ok()) {
        let mut done = false;
        let _ = window.ui.window().set_rendering_notifier(move |state, _| {
            if !done && matches!(state, slint::RenderingState::AfterRendering) {
                done = true;
                println!("first-paint-ms {:.1}", (telescope_core::time::monotonic_ns() - t0) as f64 / 1e6);
            }
        });
    }
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
    window.ui.run().map_err(|e| e.to_string())?;
    window.shut_down();
    WINDOW.with(|w| *w.borrow_mut() = None);
    Ok(())
}
