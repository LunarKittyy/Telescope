//! The tray icon on Linux (app.py's QSystemTrayIcon): a StatusNotifierItem with a dbusmenu, which Plasma, GNOME with
//! AppIndicator and most panels show. Without a StatusNotifierWatcher on the bus there's no tray, as with Qt's
//! isSystemTrayAvailable(). The menu's entries come from the shell through Tray::set_items.

use crate::icon::{TrayShows, TRAY_SIZES};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use telescope_core::system::{Tray, TrayItem};
use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// An icon as the tray protocol sends it: width, height and ARGB bytes.
type Pixmap = (i32, i32, Vec<u8>);
/// What the tray shows on hover: the icon's name, its pixmaps, the title and a description.
type ToolTip = (String, Vec<Pixmap>, String, String);

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// create_tray_icon for the protocol: every size, in ARGB bytes.
fn pixmaps(shows: TrayShows) -> Vec<Pixmap> {
    TRAY_SIZES.iter().map(|&size| {
        let argb = crate::icon::tray_rgba(size, shows).as_chunks::<4>().0.iter().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
        (size as i32, size as i32, argb)
    }).collect()
}

#[derive(Default)]
struct State {
    items: Vec<TrayItem>,
    tooltip: String,
    shows: TrayShows,
    revision: u32,
    /// Tray item ids clicked, "" for the icon itself.
    clicks: VecDeque<String>,
}

type Shared = Arc<Mutex<State>>;

struct Item(Shared);

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".into()
    }
    #[zbus(property)]
    fn id(&self) -> String {
        crate::autostart::APP_ID.into()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        "Telescope".into()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        "Active".into()
    }
    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        pixmaps(self.0.lock().unwrap().shows)
    }
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let tip = self.0.lock().unwrap().tooltip.clone();
        (String::new(), Vec::new(), tip, String::new())
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).unwrap()
    }

    /// A left click: the icon itself.
    fn activate(&self, _x: i32, _y: i32) {
        self.0.lock().unwrap().clicks.push_back(String::new());
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {}
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: String) {}

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct Menu(Shared);

fn props(item: &TrayItem) -> HashMap<String, OwnedValue> {
    let mut out: HashMap<String, OwnedValue> = HashMap::new();
    let mut put = |k: &str, v: Value| {
        if let Ok(v) = OwnedValue::try_from(v) {
            out.insert(k.into(), v);
        }
    };
    if item.title.is_empty() {
        put("type", Value::from("separator"));
    } else {
        put("label", Value::from(item.title.replace('_', "__")));
    }
    put("enabled", Value::from(item.enabled));
    put("visible", Value::from(item.visible));
    if item.checkable {
        put("toggle-type", Value::from("checkmark"));
        put("toggle-state", Value::from(i32::from(item.checked)));
    }
    out
}

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

impl Menu {
    fn layout(&self) -> (u32, Layout) {
        let st = self.0.lock().unwrap();
        let children = st.items.iter().enumerate().filter_map(|(i, item)| {
            let child: Layout = (i as i32 + 1, props(item), Vec::new());
            OwnedValue::try_from(Value::from(child)).ok()
        }).collect();
        let mut root = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(Value::from("submenu")) {
            root.insert("children-display".to_string(), v);
        }
        (st.revision, (0, root, children))
    }

    fn item_props(&self, id: i32) -> HashMap<String, OwnedValue> {
        let st = self.0.lock().unwrap();
        usize::try_from(id - 1).ok().and_then(|i| st.items.get(i)).map(props).unwrap_or_default()
    }

    fn clicked(&self, id: i32) -> bool {
        let mut st = self.0.lock().unwrap();
        let Some(item) = usize::try_from(id - 1).ok().and_then(|i| st.items.get(i)).cloned() else { return false };
        st.clicks.push_back(item.id);
        true
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, _parent_id: i32, _recursion_depth: i32, _property_names: Vec<String>) -> (u32, Layout) {
        self.layout()
    }
    fn get_group_properties(&self, ids: Vec<i32>, _property_names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        ids.into_iter().map(|id| (id, self.item_props(id))).collect()
    }
    fn get_property(&self, id: i32, name: String) -> OwnedValue {
        self.item_props(id).remove(&name).unwrap_or_else(|| OwnedValue::from(0i32))
    }
    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        if event_id == "clicked" {
            self.clicked(id);
        }
    }
    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        events.into_iter().filter(|(id, event, _, _)| event != "clicked" || !self.clicked(*id)).map(|(id, ..)| id).collect()
    }
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }
    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".into()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        "normal".into()
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(signal)]
    async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
}

pub struct SniTray {
    conn: Connection,
    shared: Shared,
}

/// A notification through the desktop's notification service, which is what Qt's showMessage did on Linux (org.freedesktop.Notifications).
pub fn desktop_message(title: &str, body: &str, urgent: bool) -> zbus::Result<()> {
    let conn = Connection::session()?;
    let icon = if urgent { "dialog-warning" } else { "dialog-information" };
    let hints: HashMap<&str, Value> = HashMap::new();
    conn.call_method(Some("org.freedesktop.Notifications"), "/org/freedesktop/Notifications", Some("org.freedesktop.Notifications"), "Notify",
                     &("Telescope", 0u32, icon, title, body, Vec::<&str>::new(), hints, 0i32))?;
    Ok(())
}

impl SniTray {
    /// The tray on the session bus, or None where nothing shows tray icons.
    pub fn new() -> Option<SniTray> {
        SniTray::on(Connection::session().ok()?)
    }

    pub fn on(conn: Connection) -> Option<SniTray> {
        let shared = Shared::default();
        {
            let server = conn.object_server();
            server.at(ITEM_PATH, Item(shared.clone())).ok()?;
            server.at(MENU_PATH, Menu(shared.clone())).ok()?;
        }
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        conn.request_name(name.as_str()).ok()?;
        let registered = conn.call_method(Some(WATCHER), "/StatusNotifierWatcher", Some(WATCHER), "RegisterStatusNotifierItem", &(name.as_str(),));
        if let Err(e) = registered {
            log::info!("No tray on this desktop: {e}");
            return None;
        }
        Some(SniTray { conn, shared })
    }
}

impl Tray for SniTray {
    fn set_items(&mut self, items: &[TrayItem]) {
        let revision = {
            let mut st = self.shared.lock().unwrap();
            if st.items == items {
                return;
            }
            st.items = items.to_vec();
            st.revision += 1;
            st.revision
        };
        if let Ok(menu) = self.conn.object_server().interface::<_, Menu>(MENU_PATH) {
            let _ = zbus::block_on(Menu::layout_updated(menu.signal_emitter(), revision, 0));
        }
    }

    fn set_tooltip(&mut self, text: &str) {
        {
            let mut st = self.shared.lock().unwrap();
            if st.tooltip == text {
                return;
            }
            st.tooltip = text.into();
        }
        if let Ok(item) = self.conn.object_server().interface::<_, Item>(ITEM_PATH) {
            let _ = zbus::block_on(Item::new_tool_tip(item.signal_emitter()));
        }
    }

    fn set_state(&mut self, camera: bool, mic: Option<bool>) {
        let shows = TrayShows::from_stream(camera, mic);
        {
            let mut st = self.shared.lock().unwrap();
            if st.shows == shows {
                return;
            }
            st.shows = shows;
        }
        if let Ok(item) = self.conn.object_server().interface::<_, Item>(ITEM_PATH) {
            let _ = zbus::block_on(Item::new_icon(item.signal_emitter()));
        }
    }

    fn poll_click(&mut self) -> Option<String> {
        self.shared.lock().unwrap().clicks.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_bus::Bus;
    use zbus::blocking::MessageIterator;
    use zbus::message::Type;

    /// Plasma's watcher, as far as registering goes: remembers who registered.
    fn watcher(bus: &Bus) -> Arc<Mutex<Vec<String>>> {
        let conn = bus.connect().unwrap();
        conn.request_name(WATCHER).unwrap();
        let registered = Arc::new(Mutex::new(Vec::new()));
        let seen = registered.clone();
        std::thread::spawn(move || {
            for msg in MessageIterator::from(&conn).flatten() {
                if msg.message_type() == Type::MethodCall {
                    if let Ok((name,)) = msg.body().deserialize::<(String,)>() {
                        seen.lock().unwrap().push(name);
                    }
                    let _ = conn.send(&zbus::Message::method_return(&msg.header()).unwrap().build(&()).unwrap());
                }
            }
        });
        registered
    }

    fn item(i: &str, title: &str) -> TrayItem {
        TrayItem { id: i.into(), title: title.into(), checkable: false, checked: false, visible: true, enabled: true }
    }

    #[test]
    fn no_watcher_means_no_tray() {
        let Some(bus) = Bus::start() else { return };
        assert!(SniTray::on(bus.connect().unwrap()).is_none());
    }

    #[test]
    fn the_menu_and_the_icon_report_their_clicks() {
        let Some(bus) = Bus::start() else { return };
        let registered = watcher(&bus);
        let mut tray = SniTray::on(bus.connect().unwrap()).unwrap();
        let name = registered.lock().unwrap()[0].clone();
        assert!(name.starts_with("org.kde.StatusNotifierItem-"));
        tray.set_items(&[item("show", "Show"), item("sep", ""), item("quit", "Quit")]);
        tray.set_tooltip("Telescope: streaming the camera");

        let panel = bus.connect().unwrap();
        let reply = panel.call_method(Some(name.as_str()), MENU_PATH, Some("com.canonical.dbusmenu"), "GetLayout", &(0i32, -1i32, Vec::<String>::new())).unwrap();
        let (revision, (_, _, children)): (u32, Layout) = reply.body().deserialize().unwrap();
        assert_eq!((revision, children.len()), (1, 3));
        let reply = panel.call_method(Some(name.as_str()), MENU_PATH, Some("com.canonical.dbusmenu"), "GetGroupProperties", &(vec![1i32, 2], Vec::<String>::new())).unwrap();
        let groups: Vec<(i32, HashMap<String, OwnedValue>)> = reply.body().deserialize().unwrap();
        assert_eq!(String::try_from(groups[0].1["label"].try_clone().unwrap()).unwrap(), "Show");
        assert_eq!(String::try_from(groups[1].1["type"].try_clone().unwrap()).unwrap(), "separator");

        panel.call_method(Some(name.as_str()), MENU_PATH, Some("com.canonical.dbusmenu"), "Event", &(3i32, "clicked", Value::from(0i32), 0u32)).unwrap();
        panel.call_method(Some(name.as_str()), ITEM_PATH, Some("org.kde.StatusNotifierItem"), "Activate", &(0i32, 0i32)).unwrap();
        assert_eq!(tray.poll_click().as_deref(), Some("quit"));
        assert_eq!(tray.poll_click().as_deref(), Some(""));
        assert_eq!(tray.poll_click(), None);

        let reply = panel.call_method(Some(name.as_str()), ITEM_PATH, Some("org.freedesktop.DBus.Properties"), "Get", &("org.kde.StatusNotifierItem", "ToolTip")).unwrap();
        let tip: OwnedValue = reply.body().deserialize().unwrap();
        let (_, _, title, _): ToolTip = tip.try_into().unwrap();
        assert_eq!(title, "Telescope: streaming the camera");
    }
}
