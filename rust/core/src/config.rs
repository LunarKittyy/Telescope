//! This computer's identity and its paired phones, kept as JSON next to (not in) the Python app's config.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Phone {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub computer_id: String,
    pub computer_name: String,
    #[serde(default)]
    pub phones: Vec<Phone>,
    #[serde(default)]
    pub selected_phone: Option<String>,
}

pub fn folder() -> PathBuf {
    if let Some(dir) = std::env::var_os("TELESCOPE_RS_PROFILE") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("telescope-rs")
}

impl Config {
    pub fn load() -> Config {
        let path = folder().join("config.json");
        if let Ok(raw) = std::fs::read(&path) {
            if let Ok(cfg) = serde_json::from_slice::<Config>(&raw) {
                return cfg;
            }
        }
        Config {
            computer_id: crate::util::random_hex(16),
            computer_name: crate::util::hostname(),
            phones: Vec::new(),
            selected_phone: None,
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = folder();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap())?;
        std::fs::rename(tmp, dir.join("config.json"))
    }

    pub fn selected(&self) -> Option<&Phone> {
        let id = self.selected_phone.as_deref();
        self.phones.iter().find(|p| Some(p.id.as_str()) == id).or(self.phones.first())
    }

    /// Pairing again from the same phone replaces its entry.
    pub fn add_phone(&mut self, phone: Phone) {
        self.selected_phone = Some(phone.id.clone());
        self.phones.retain(|p| p.id != phone.id);
        self.phones.push(phone);
    }
}
