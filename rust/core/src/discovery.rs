//! Finds paired phones on the LAN by the _telescope._tcp announcement their app makes (discovery.py). The TXT "id"
//! is the phone id; the address is only a hint, since every connection still checks the pin.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SERVICE_TYPE: &str = "_telescope._tcp.local.";

#[derive(Default)]
pub struct LanDiscovery {
    by_id: Arc<Mutex<HashMap<String, Vec<String>>>>,
    daemon: Option<ServiceDaemon>,
}

impl LanDiscovery {
    pub fn start() -> LanDiscovery {
        let mut me = LanDiscovery::default();
        let Ok(daemon) = ServiceDaemon::new() else { return me };
        let Ok(events) = daemon.browse(SERVICE_TYPE) else { return me };
        let by_id = me.by_id.clone();
        let mut by_name: HashMap<String, String> = HashMap::new();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(id) = info.get_property_val_str("id").map(str::to_string) else { continue };
                        let ips: Vec<String> = info.get_addresses_v4().iter().map(|a| a.to_string()).collect();
                        by_name.insert(info.get_fullname().to_string(), id.clone());
                        by_id.lock().unwrap().insert(id, ips);
                    }
                    ServiceEvent::ServiceRemoved(_, name) => {
                        if let Some(id) = by_name.remove(&name) {
                            by_id.lock().unwrap().remove(&id);
                        }
                    }
                    _ => {}
                }
            }
        });
        me.daemon = Some(daemon);
        me
    }

    pub fn lookup(&self, phone_id: &str) -> Vec<String> {
        self.by_id.lock().unwrap().get(phone_id).cloned().unwrap_or_default()
    }
}

impl Drop for LanDiscovery {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            let _ = d.shutdown();
        }
    }
}
