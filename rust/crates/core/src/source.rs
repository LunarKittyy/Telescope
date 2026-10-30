//! What a stream comes from: a paired phone, or a plugin's StreamSource (sources.py).

use crate::media::{Codec, Decoder, VideoFeed};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// A paired phone as plugin_configs.connection.phones keeps it in the settings file (field for field).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairedPhone {
    pub id: String,
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub ips: Vec<String>,
    #[serde(default)]
    pub active_ip: Option<String>,
    #[serde(default)]
    pub cert_sha256: String,
}

/// How a phone is reached for one stream; plain data so a plugin can ask for a hand-over (HostStreams.hand_over_stream).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteSpec {
    /// Over the cable through adb, by the device's serial.
    Usb { serial: String },
    /// Over the network at this address (Wi-Fi, or Tailscale's 100.64/10).
    Network { address: String },
}

/// A source's identity: phones are keyed by their id, plugin sources by an id that doesn't look like one ("browser:...").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Phone,
    Plugin,
}

/// Talking to whatever a stream comes from while it runs, the role PhoneControlClient plays (ctrl in the Python hooks).
pub trait ControlClient: Send + Sync {
    /// One /v1/control body, e.g. {"action": "torch", "value": 1}.
    fn send(&self, body: &Value) -> Result<(), String>;
    /// The source's /v1/state.
    fn state(&self) -> Result<Value, String>;
    /// Whether there's a microphone to bring over.
    fn has_audio(&self) -> bool;
}

/// Something other than a paired phone to stream from, like the Browser camera (StreamSource in plugin.py).
pub trait StreamSource: Send + Sync {
    /// Also the key its per-device settings are saved under, so it mustn't look like a phone id.
    fn id(&self) -> String;
    fn name(&self) -> String;
    /// What stream_started and on_stream_start get as the stream's URL.
    fn url(&self) -> String;
    /// On the host's thread, at Start: get ready, or show a banner and return false.
    fn prepare(&self, interactive: bool) -> bool;
    /// A fresh feed of its video, and what codec it's in.
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String>;
    fn control_client(&self) -> Option<Box<dyn ControlClient>>;
    /// The rate the virtual camera runs at.
    fn fps(&self) -> u32;
    /// At Start with nothing streaming, another source's id to stream instead.
    fn redirect(&self) -> Option<String> {
        None
    }
    /// Its settings are only saved where they differ from the defaults.
    fn remember_changes_only(&self) -> bool {
        false
    }
    /// The virtual camera's size when Setup's canvas is Auto; None takes the first frame's.
    fn auto_canvas(&self) -> Option<(u32, u32)> {
        None
    }
    /// What to check when it can't be reached for a while.
    fn lost_hint(&self) -> String {
        String::new()
    }
    /// A decoder for packets only the source itself understands (a webcam's raw frames); None: media decodes them as
    /// open_feed's codec says. Asked each time a feed opens, after open_feed.
    fn decoder(&self) -> Option<Box<dyn Decoder>> {
        None
    }
    /// prepare() said no, but something is on its way to it (a browser to connect): next to other streams, it keeps a
    /// tile until then.
    fn waiting(&self) -> bool {
        false
    }
    /// Its waiting tile was closed.
    fn cancel_wait(&self) {}
    /// Why it isn't streaming yet while the stream waits to try again (WebcamReader.waiting_text), "" for the usual.
    fn waiting_text(&self) -> String {
        String::new()
    }
}

/// The Connection section, read field by field so whatever else it holds (route, udp, newer keys) is written back as it was.
#[derive(Clone, Debug, PartialEq)]
pub struct Phones {
    pub computer_id: String,
    pub computer_name: String,
    pub phones: Vec<PairedPhone>,
    pub selected_phone: Option<String>,
    rest: Map<String, Value>,
}

impl Phones {
    /// From settings["plugin_configs"]["connection"]; identity() makes up a computer id and name when there aren't any yet.
    pub fn from_section(section: Option<&Value>, identity: impl FnOnce() -> (String, String)) -> Phones {
        let mut rest = section.and_then(Value::as_object).cloned().unwrap_or_default();
        let text = |rest: &mut Map<String, Value>, key: &str| match rest.shift_remove(key) {
            Some(Value::String(s)) if !s.is_empty() => Some(s),
            _ => None,
        };
        let (id, name) = (text(&mut rest, "computer_id"), text(&mut rest, "computer_name"));
        let (computer_id, computer_name) = match (id, name) {
            (Some(id), Some(name)) => (id, name),
            (id, name) => {
                let (new_id, new_name) = identity();
                (id.unwrap_or(new_id), name.unwrap_or(new_name))
            }
        };
        let phones = rest.shift_remove("phones").and_then(|p| p.as_array().cloned()).unwrap_or_default()
            .into_iter().filter_map(|p| serde_json::from_value(p).ok()).collect();
        let selected_phone = text(&mut rest, "selected_phone");
        Phones { computer_id, computer_name, phones, selected_phone, rest }
    }

    /// The section to save, in the order Connection writes it.
    pub fn to_section(&self) -> Value {
        let mut out = Map::new();
        out.insert("computer_id".into(), json!(self.computer_id));
        out.insert("computer_name".into(), json!(self.computer_name));
        for (k, v) in &self.rest {
            out.insert(k.clone(), v.clone());
        }
        out.insert("phones".into(), serde_json::to_value(&self.phones).unwrap_or_default());
        out.insert("selected_phone".into(), self.selected_phone.clone().map_or(Value::Null, Value::String));
        Value::Object(out)
    }

    pub fn selected(&self) -> Option<&PairedPhone> {
        let id = self.selected_phone.as_deref();
        self.phones.iter().find(|p| Some(p.id.as_str()) == id).or(self.phones.first())
    }

    /// Pairing again from the same phone replaces its entry.
    pub fn add_phone(&mut self, phone: PairedPhone) {
        self.selected_phone = Some(phone.id.clone());
        self.phones.retain(|p| p.id != phone.id);
        self.phones.push(phone);
    }
}

#[cfg(test)]
mod phones_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn section_round_trips_with_unknown_keys() {
        let section = json!({"computer_id": "c", "computer_name": "desk", "route": "auto", "udp": false,
            "phones": [{"id": "p", "name": "Pixel", "token": "t", "ips": ["1.2.3.4"], "active_ip": "1.2.3.4", "cert_sha256": "aa"}],
            "selected_phone": "p"});
        let phones = Phones::from_section(Some(&section), || unreachable!());
        assert_eq!(phones.selected().unwrap().name, "Pixel");
        assert_eq!(phones.to_section(), section);
    }
}
