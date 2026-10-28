//! Plugins people add, behind core's Plugin trait: a runtime (Lua now, WASM later) implements Guest, sees plain JSON both ways and reaches the host only through call_host.

use serde_json::{json, Value};
use telescope_core::host::Host;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::ui::{CardSpec, UiEvent};
use telescope_core::Event;

/// A plugin running in a runtime: every hook is a name and JSON arguments, every answer JSON.
pub trait Guest: Send {
    /// Runs hook (a Plugin method's name, like "on_event") with args; host calls made meanwhile go through call_host.
    fn call(&mut self, host: &mut dyn Host, hook: &str, args: Value) -> Result<Value, String>;
    /// The hooks and events the plugin defined, so nothing crosses over for the rest.
    fn defines(&self, hook: &str) -> bool;
}

/// Any guest as a core Plugin. info comes from the plugin's plugin.json.
pub struct GuestPlugin<G: Guest> {
    pub info: PluginInfo,
    pub guest: G,
    config: Value,
}

impl<G: Guest> GuestPlugin<G> {
    pub fn new(info: PluginInfo, guest: G) -> Self {
        GuestPlugin { info, guest, config: json!({}) }
    }

    fn hook(&mut self, host: &mut dyn Host, hook: &str, args: Value) -> Option<Value> {
        if !self.guest.defines(hook) {
            return None;
        }
        match self.guest.call(host, hook, args) {
            Ok(v) => Some(v),
            Err(e) => {
                log::error!("Plugin {} failed in {hook}: {e}", self.info.name);
                None
            }
        }
    }
}

impl<G: Guest> Plugin for GuestPlugin<G> {
    fn info(&self) -> PluginInfo {
        self.info.clone()
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.hook(host, "setup", json!({}));
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        if self.guest.defines(event.name()) {
            let args = serde_json::to_value(event).unwrap_or_default();
            self.hook(host, event.name(), args);
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.hook(host, "on_stream_start", serde_json::to_value(stream).unwrap_or_default());
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.hook(host, "on_stream_stop", json!({}));
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.hook(host, "on_ui", serde_json::to_value(event).unwrap_or_default());
    }

    fn card(&mut self) -> Option<CardSpec> {
        None
    }

    fn get_config(&self) -> Value {
        self.config.clone()
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.config = cfg.clone();
        Ok(())
    }
}

/// The one door from a guest into the host: a method name from core's Host traits and its arguments as JSON.
pub fn call_host(host: &mut dyn Host, method: &str, args: &Value) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    match method {
        "schedule_save" => host.schedule_save(),
        "is_streaming" => return Ok(json!(host.is_streaming())),
        "focused_source_id" => return Ok(json!(host.focused_source_id())),
        "phone_control" => host.phone_control(s("source_id"), args.get("body").unwrap_or(&Value::Null)),
        "set_ui_props" => host.set_ui_props(s("ui").unwrap_or("card"), args.get("props").unwrap_or(&Value::Null)),
        "send_notification" => host.send_notification(s("title").unwrap_or(""), s("body").unwrap_or(""), true),
        "emit" => {
            let event: Event = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            host.emit(event);
        }
        // The rest of core's Host methods follow the same pattern; the plugin-host session fills them in
        other => return Err(format!("no host method {other}")),
    }
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;

    struct Echo;

    impl Guest for Echo {
        fn call(&mut self, host: &mut dyn Host, hook: &str, args: Value) -> Result<Value, String> {
            call_host(host, "phone_control", &json!({"body": {"action": "torch", "value": 1}}))?;
            Ok(json!({"hook": hook, "args": args}))
        }

        fn defines(&self, hook: &str) -> bool {
            hook == "stream_started"
        }
    }

    #[test]
    fn a_guest_only_hears_what_it_defines() {
        let mut p = GuestPlugin::new(PluginInfo { name: "echo".into(), ..Default::default() }, Echo);
        let mut host = FakeHost::new();
        p.on_event(&mut host, &Event::StreamStopped);
        assert!(host.calls.is_empty());
        p.on_event(&mut host, &Event::StreamStarted { url: "u".into() });
        assert_eq!(host.calls, ["phone_control None {\"action\":\"torch\",\"value\":1}"]);
    }
}
