//! Testing a plugin without Telescope: drive its hooks against a host that writes everything down, and check it the way
//! check_plugin.py checked Python plugins. `check-plugin path/to/plugin` runs check() from the command line.

use crate::load::{load_unchecked, LuaPlugin};
use crate::manifest::{self, MANIFEST};
use crate::package::{Installed, PluginFolder};
use crate::switches::Switches;
use serde_json::{json, Value};
use std::path::Path;
use telescope_core::frame::{FrameStep, ShaderStep};
use telescope_core::plugin::{Plugin, StreamInfo};
use telescope_core::testing::FakeHost;
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::Event;

/// Reads a plugin folder the way Telescope reads one being written: on, whatever its fingerprint, hidden files left out.
pub fn inspect(folder: &Path) -> Installed {
    let parent = folder.parent().unwrap_or(Path::new("."));
    PluginFolder::new(parent, Switches::default(), []).with_dev_folders(vec![folder.to_path_buf()])
        .find_installed().into_iter().find(|i| i.folder == folder).unwrap_or_default()
}

/// Whether a shader compiles as the GPU executor will compile it (core::shader::assemble), with errors pointing into the plugin's file.
pub fn validate_shader(step: &ShaderStep, file: &str) -> Result<(), String> {
    let module = telescope_core::shader::assemble(step).map_err(|e| format!("{file}: {e}"))?;
    let offset = module.find(&step.wgsl).map_or(0, |at| module[..at].lines().count());
    let place = |line: Option<u32>| match line {
        Some(l) if l as usize > offset => format!("{file}:{}", l as usize - offset),
        _ => file.to_string(),
    };
    let parsed = naga::front::wgsl::parse_str(&module).map_err(|e| format!("{}: {}", place(e.location(&module).map(|l| l.line_number)), e.message()))?;
    let mut validator = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty());
    validator.validate(&parsed).map_err(|e| format!("{}: {}", place(e.location(&module).map(|l| l.line_number)), e.as_inner()))?;
    Ok(())
}

/// A plugin loaded from its folder with a FakeHost, for tests. Every hook goes through the same GuestPlugin Telescope uses.
pub struct PluginTest {
    pub installed: Installed,
    pub plugin: LuaPlugin,
    pub host: FakeHost,
}

impl PluginTest {
    /// The plugin in folder, in English.
    pub fn open(folder: impl AsRef<Path>) -> Result<PluginTest, String> {
        PluginTest::open_in(folder, "en")
    }

    /// The plugin in folder with its texts in lang (its locale/<lang>.po).
    pub fn open_in(folder: impl AsRef<Path>, lang: &str) -> Result<PluginTest, String> {
        let installed = inspect(folder.as_ref());
        if !installed.problem.is_empty() {
            return Err(installed.problem);
        }
        let plugin = load_unchecked(&installed, lang)?;
        Ok(PluginTest { installed, plugin, host: FakeHost::new() })
    }

    /// Runs a hook directly, with an error if it raised one (Telescope would log it and carry on).
    pub fn call(&mut self, hook: &str, args: Vec<Value>) -> Result<Value, String> {
        self.plugin.call(Some(&mut self.host), hook, args)
    }

    /// Runs a hook core calls without a host (card, frame_step, get_config...).
    pub fn call_without_host(&mut self, hook: &str, args: Vec<Value>) -> Result<Value, String> {
        self.plugin.call(None, hook, args)
    }

    pub fn defines(&self, hook: &str) -> bool {
        use crate::guest::Guest;
        self.plugin_guest(|g| g.defines(hook))
    }

    fn plugin_guest<R>(&self, f: impl FnOnce(&crate::lua::LuaGuest) -> R) -> R {
        f(&self.plugin.guest_ref())
    }

    pub fn setup(&mut self) {
        self.plugin.setup(&mut self.host);
    }

    pub fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
    }

    /// A callback in the plugin's UI fired, like the shell reports it.
    pub fn ui(&mut self, ui: &str, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: ui.into(), callback: callback.into(), args });
    }

    pub fn start_stream(&mut self, source_id: &str) {
        self.host.streaming = true;
        self.host.focused = Some(source_id.to_string());
        self.plugin.on_stream_starting(&mut self.host);
        self.plugin.on_stream_start(&mut self.host, &StreamInfo { source_id: source_id.into(), url: format!("udp://{source_id}"), session: 0 });
        self.plugin.on_event(&mut self.host, &Event::StreamStarted { url: format!("udp://{source_id}") });
        self.plugin.on_event(&mut self.host, &Event::StreamConnected { source_id: source_id.into() });
    }

    pub fn stop_stream(&mut self) {
        self.host.streaming = false;
        self.plugin.on_stream_stop(&mut self.host);
        self.plugin.on_event(&mut self.host, &Event::StreamStopped);
    }

    pub fn card(&mut self) -> Option<CardSpec> {
        self.plugin.card()
    }

    pub fn config(&self) -> Value {
        self.plugin.get_config()
    }

    pub fn set_config(&mut self, cfg: Value) -> Result<(), String> {
        self.plugin.set_config(&cfg).map_err(|e| e.0)
    }

    /// The plugin's frame step now, as the GPU executor gets it.
    pub fn shader(&mut self) -> Option<ShaderStep> {
        match self.plugin.frame_step()? {
            FrameStep::Shader(s) | FrameStep::Either(s, _) => Some(s),
            FrameStep::Cpu(_) => None,
        }
    }

    /// What the plugin asked of the host since the last time, emptied.
    pub fn take_calls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.host.calls)
    }

    /// The events the plugin emitted since the last time, emptied.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.host.events)
    }
}

/// What check() found: each step with what went wrong, if anything.
#[derive(Debug, Default)]
pub struct Report {
    pub steps: Vec<(String, Result<(), String>)>,
    pub notes: Vec<String>,
}

impl Report {
    fn step(&mut self, what: &str, result: Result<(), String>) {
        self.steps.push((what.to_string(), result));
    }

    pub fn problems(&self) -> usize {
        self.steps.iter().filter(|(_, r)| r.is_err()).count()
    }

    /// The report as check-plugin prints it.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (what, result) in &self.steps {
            match result {
                Ok(()) => out += &format!("[ OK ] {what}\n"),
                Err(e) => out += &format!("[FAIL] {what}\n{}\n", indent(e)),
            }
        }
        for note in &self.notes {
            out += &format!("[NOTE] {note}\n");
        }
        out += &match self.problems() {
            0 => "All good.\n".to_string(),
            1 => "1 problem to fix.\n".to_string(),
            n => format!("{n} problems to fix.\n"),
        };
        out
    }
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("       {l}")).collect::<Vec<_>>().join("\n")
}

/// The .slint file of a UI spec, and whether it exports the component.
fn check_ui(spec: &UiSpec) -> Result<(), String> {
    let SlintSource::File(path) = &spec.source else { return Ok(()) };
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let exported = text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("export component ") && l["export component ".len()..].split_whitespace().next() == Some(spec.component.as_str())
    });
    if exported { Ok(()) } else { Err(format!("{path} has no `export component {}`", spec.component)) }
}

/// Everything check_plugin.py checked, for a Lua plugin: it loads, its card and shader are usable, its settings save and come back,
/// it gets through a stream starting and stopping, and its translations can be read.
pub fn check(folder: &Path) -> Report {
    let mut report = Report::default();
    let installed = inspect(folder);
    if !installed.problem.is_empty() {
        report.step("plugin.json and files", Err(installed.problem.clone()));
        return report;
    }
    report.step("plugin.json and files", Ok(()));
    check_repo(&installed, &mut report);
    let mut t = match load_unchecked(&installed, "en") {
        Ok(plugin) => PluginTest { installed: installed.clone(), plugin, host: FakeHost::new() },
        Err(e) => {
            report.step(&format!("{} runs", installed.main), Err(e));
            return report;
        }
    };
    report.step(&format!("{} runs", installed.main), Ok(()));
    let hook = |t: &mut PluginTest, report: &mut Report, name: &str, args: Vec<Value>, with_host: bool| -> Option<Value> {
        if !t.defines(name) {
            return None;
        }
        let result = if with_host { t.call(name, args) } else { t.call_without_host(name, args) };
        let answer = result.as_ref().ok().cloned();
        report.step(&format!("{name}()"), result.map(|_| ()));
        answer
    };
    hook(&mut t, &mut report, "setup", vec![], true);
    if let Some(v) = hook(&mut t, &mut report, "card", vec![], false) {
        let spec = t.plugin.ui_spec(&v);
        report.step("the card's .slint file", spec.and_then(|s| check_ui(&s)));
    }
    if let Some(v) = hook(&mut t, &mut report, "header_widget", vec![], false) {
        report.step("the header widget's .slint file", t.plugin.ui_spec(&v).and_then(|s| check_ui(&s)));
    }
    if let Some(Value::Array(sections)) = hook(&mut t, &mut report, "settings_sections", vec![], false) {
        for s in sections {
            report.step("a settings section's .slint file", t.plugin.ui_spec(&s).and_then(|s| check_ui(&s)));
        }
    }
    hook(&mut t, &mut report, "actions", vec![], false);
    hook(&mut t, &mut report, "tray_actions", vec![], false);
    hook(&mut t, &mut report, "on_plugins_ready", vec![], true);
    check_shader(&mut t, &mut report);
    t.plugin_guest(|g| g.set_strict_config(true));
    let config = hook(&mut t, &mut report, "get_config", vec![], false);
    t.plugin_guest(|g| g.set_strict_config(false));
    if let Some(cfg) = config {
        let cfg = if cfg.is_object() { cfg } else { json!({}) };
        if t.defines("set_config") {
            let back = t.call_without_host("set_config", vec![cfg.clone()]).and_then(|_| {
                let again = t.config();
                if again == cfg { Ok(()) } else { Err(format!("get_config() gave {cfg} before set_config() and {again} after")) }
            });
            report.step("settings come back after a save", back);
        }
        if let Err(e) = hook_with_config(&mut t) {
            report.step("set_config() with settings from an older version", Err(e));
        }
    }
    check_shader(&mut t, &mut report);
    let stream = json!({"source_id": "phone", "url": "udp://phone"});
    t.host.streaming = true;
    hook(&mut t, &mut report, "on_stream_starting", vec![], true);
    hook(&mut t, &mut report, "on_stream_start", vec![stream], true);
    for event in [Event::StreamStarted { url: "udp://phone".into() }, Event::StreamConnected { source_id: "phone".into() },
                  Event::StreamLost { source_id: "phone".into() }, Event::StreamStopped] {
        let name = event.name();
        if t.plugin_guest(|g| crate::guest::Guest::listens(g, name)) {
            let result = t.call("on_event", vec![serde_json::to_value(&event).unwrap_or_default()]);
            report.step(&format!("events.{name}()"), result.map(|_| ()));
        }
    }
    hook(&mut t, &mut report, "on_stream_stop", vec![], true);
    t.host.streaming = false;
    hook(&mut t, &mut report, "diagnostics", vec![], false);
    hook(&mut t, &mut report, "shutdown", vec![], true);
    check_shader_files(folder, &mut report);
    check_catalogs(folder, &mut report);
    report
}

/// set_config() with nothing saved yet and with values of the wrong type, as older versions or hand edits leave them.
fn hook_with_config(t: &mut PluginTest) -> Result<(), String> {
    if !t.defines("set_config") {
        return Ok(());
    }
    t.call_without_host("set_config", vec![json!({})])?;
    let cfg = t.config();
    let odd: serde_json::Map<String, Value> = cfg.as_object().into_iter().flatten().map(|(k, _)| (k.clone(), json!("?"))).collect();
    // Rejecting what doesn't fit is fine (Telescope then keeps what loads); failing on it isn't
    let _ = t.call_without_host("set_config", vec![Value::Object(odd)]);
    t.call_without_host("set_config", vec![cfg]).map(|_| ())
}

fn check_shader(t: &mut PluginTest, report: &mut Report) {
    if !t.defines("frame_step") {
        return;
    }
    match t.call_without_host("frame_step", vec![]) {
        Err(e) => report.step("frame_step()", Err(e)),
        Ok(Value::Null) => {}
        Ok(v) => {
            let file = v.get("shader").and_then(Value::as_str).unwrap_or("the shader").to_string();
            let result = t.plugin.shader_step(&v).and_then(|step| validate_shader(&step, &file));
            if !report.steps.iter().any(|(what, _)| what == &format!("{file} compiles")) {
                report.step(&format!("{file} compiles"), result);
            }
        }
    }
}

/// Every .wgsl file in the plugin compiles, whether or not a frame step used it during the check (uniforms all 0).
fn check_shader_files(folder: &Path, report: &mut Report) {
    let Ok(files) = crate::package::files(folder, true) else { return };
    for rel in files.iter().filter(|p| p.extension().is_some_and(|e| e == "wgsl")) {
        let name = rel.to_string_lossy().replace('\\', "/");
        let what = format!("{name} compiles");
        if report.steps.iter().any(|(w, _)| *w == what) {
            continue;
        }
        let result = std::fs::read_to_string(folder.join(rel)).map_err(|e| e.to_string())
            .and_then(|wgsl| validate_shader(&ShaderStep { wgsl, uniforms: json!({}), textures: Vec::new() }, &name));
        report.step(&what, result);
    }
}

/// The note check_plugin.check_repo gave about "repo" and "version".
pub fn check_repo(installed: &Installed, report: &mut Report) {
    let problem = manifest::repo_problem(installed.manifest.get("repo"));
    if !problem.is_empty() {
        report.step(MANIFEST, Err(format!("{problem} It isn't shown until it is.")));
        return;
    }
    if installed.repo.is_empty() {
        return;
    }
    if crate::updates::github_repo(&installed.repo).is_none() {
        report.notes.push("The update check only follows https://github.com/owner/repo addresses, so people won't hear about new versions from this one.".into());
    } else if installed.version.is_empty() {
        report.notes.push("Give it a \"version\" in plugin.json, so Telescope can tell people when a new release is out.".into());
    } else {
        report.notes.push("Telescope can tell people when a new release of it is out on GitHub (rust/docs/writing-plugins.md, Sharing it).".into());
    }
}

/// Each locale/*.po reads, and no translation has a {field} its English doesn't.
fn check_catalogs(folder: &Path, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(folder.join(crate::i18n::LOCALE)) else { return };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")).collect();
    paths.sort();
    for path in paths {
        let name = format!("locale/{}", path.file_name().unwrap_or_default().to_string_lossy());
        let result = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| crate::i18n::Catalog::parse(&b))
            .and_then(|c| match c.field_mismatches().first() {
                Some(msgid) => Err(format!("the translation of {msgid:?} has a {{field}} the English doesn't, so it shows in English")),
                None => Ok(()),
            });
        report.step(&name, result);
    }
}
