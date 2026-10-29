//! Linux global shortcuts through the GlobalShortcuts desktop portal (platform/hotkeys_portal.py): Plasma 5.27+,
//! GNOME 48+, Hyprland.
//!
//! Talks D-Bus on threads of its own. The flow: register the app id (an app outside Flatpak has to, before any other
//! portal call), create a session, bind the shortcuts to it. The desktop then asks the user to confirm them, and may
//! bind other keys than the ones suggested: the answer says which, and ShortcutsChanged says when that changes later.
//! A session can only bind once, so new bindings get a new session.

use crate::hotkeys::{Hotkey, HotkeyBackend, HotkeyStatus, Signals};
use crate::keys;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use telescope_core::i18n::tr;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::MatchRule;

const BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
// Plasma's shortcut store, which the portal binds into on Plasma
const KGLOBALACCEL: &str = "org.kde.kglobalaccel";
const WAIT_STEP: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Starting,
    Ready,
    /// The desktop's dialog is up.
    Asking,
    Unavailable,
}

struct Inner {
    state: State,
    why: String,
    statuses: HashMap<String, HotkeyStatus>,
    version: u32,
    session: Option<String>,
}

struct Shared {
    inner: Mutex<Inner>,
    stopping: AtomicBool,
    signals: Signals,
}

impl Shared {
    fn set_state(&self, state: State, why: &str) {
        {
            let mut i = self.inner.lock().unwrap();
            i.state = state;
            i.why = why.into();
        }
        self.signals.changed();
    }
}

enum Cmd {
    Bind(Vec<Hotkey>),
    Configure,
    Stop,
}

pub type Connect = Box<dyn Fn() -> zbus::Result<Connection> + Send>;

pub struct PortalHotkeys {
    cmds: Sender<Cmd>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// Whether trying the portal makes sense: there's a session bus.
pub fn portal_possible() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|a| !a.is_empty()) {
        return true;
    }
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| std::path::Path::new(&d).join("bus").exists())
}

impl PortalHotkeys {
    /// connect opens the bus; prepare runs on the thread before registering (it makes sure the menu entry is there).
    pub fn new(app_id: &str, signals: Signals, connect: Connect, prepare: Option<Box<dyn FnOnce() + Send>>) -> PortalHotkeys {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner { state: State::Starting, why: String::new(), statuses: HashMap::new(), version: 0, session: None }),
            stopping: AtomicBool::new(false),
            signals,
        });
        let worker = Worker { app_id: app_id.into(), shared: shared.clone(), connect, prepare, cmds: rx };
        let thread = std::thread::Builder::new().name("portal-shortcuts".into()).spawn(move || worker.run()).ok();
        PortalHotkeys { cmds: tx, shared, thread }
    }

    pub fn can_configure(&self) -> bool {
        let i = self.shared.inner.lock().unwrap();
        i.version >= 2 && i.session.is_some() && i.state == State::Ready
    }
}

impl HotkeyBackend for PortalHotkeys {
    fn apply(&mut self, hotkeys: &[Hotkey]) {
        let _ = self.cmds.send(Cmd::Bind(hotkeys.to_vec()));
    }

    fn status(&self, id: &str) -> Option<HotkeyStatus> {
        let i = self.shared.inner.lock().unwrap();
        match i.state {
            State::Unavailable => Some(HotkeyStatus::new(false, tr("Not available on this desktop"))),
            State::Asking => Some(HotkeyStatus::new(false, tr("Waiting for you to confirm it"))),
            _ => i.statuses.get(id).cloned(),
        }
    }

    fn note(&self) -> String {
        let (state, why) = {
            let i = self.shared.inner.lock().unwrap();
            (i.state, i.why.clone())
        };
        if state == State::Unavailable {
            let first = if why.is_empty() { tr("This desktop doesn't offer global shortcuts to apps.") } else {
                tr("This desktop doesn't offer global shortcuts to apps ({why}).").replace("{why}", &why)
            };
            return first + " " + &tr("Bind a Copy command line in its own keyboard settings instead.");
        }
        tr("Your desktop asks you to confirm global shortcuts, and you can change their keys in its own settings. The key it settled on shows next to each one.")
    }

    fn system_picks_keys(&self) -> bool {
        true
    }

    fn can_rebind(&self) -> bool {
        let i = self.shared.inner.lock().unwrap();
        let unconfirmed = tr("Not confirmed in the desktop's dialog");
        i.state == State::Ready && i.statuses.values().any(|s| !s.working && s.text == unconfirmed)
    }

    fn configure(&mut self) -> bool {
        if !self.can_configure() {
            return false;
        }
        self.cmds.send(Cmd::Configure).is_ok()
    }

    fn name(&self) -> &'static str {
        "PortalHotkeys"
    }

    fn stop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = self.cmds.send(Cmd::Stop);
        if let Some(thread) = self.thread.take() {
            // Up to about one wait step, as Python's join(timeout=2)
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Drop for PortalHotkeys {
    fn drop(&mut self) {
        self.stop();
    }
}

fn status_of(props: Option<&HashMap<String, OwnedValue>>) -> HotkeyStatus {
    let Some(props) = props else { return HotkeyStatus::new(false, tr("Not confirmed in the desktop's dialog")) };
    let trigger = props.get("trigger_description").and_then(|v| text(v)).unwrap_or_default();
    if trigger.is_empty() {
        return HotkeyStatus::new(false, tr("No key set in the desktop's shortcut settings"));
    }
    HotkeyStatus::new(true, tr("Everywhere: {keys}").replace("{keys}", &trigger))
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        Value::Value(inner) => text(inner),
        _ => None,
    }
}

/// a(sa{sv}) as (id, props) pairs.
fn shortcut_list(v: &Value) -> Vec<(String, HashMap<String, OwnedValue>)> {
    let v = match v { Value::Value(inner) => inner.as_ref(), v => v };
    let Value::Array(items) = v else { return Vec::new() };
    items.iter().filter_map(|item| {
        let Value::Structure(s) = item else { return None };
        let [Value::Str(id), Value::Dict(d)] = s.fields() else { return None };
        let props: HashMap<String, OwnedValue> = d.try_clone().ok()?.try_into().ok()?;
        Some((id.to_string(), props))
    }).collect()
}

struct Stopped;

struct Worker {
    app_id: String,
    shared: Arc<Shared>,
    connect: Connect,
    prepare: Option<Box<dyn FnOnce() + Send>>,
    cmds: Receiver<Cmd>,
}

impl Worker {
    fn run(mut self) {
        // Nothing goes to the portal until there's a global shortcut to bind: no session, no dialog, no menu entry
        let mut tidied = false;
        let first = loop {
            match self.cmds.recv() {
                Ok(Cmd::Bind(h)) if !h.is_empty() => break h,
                Ok(Cmd::Bind(_)) if !tidied => {
                    // None to bind, but Plasma may still list ones from before
                    tidied = true;
                    match (self.connect)() {
                        Ok(conn) => self.forget_unbound(&conn),
                        Err(e) => log::debug!("Couldn't reach the session bus to tidy shortcuts: {e}"),
                    }
                }
                Ok(Cmd::Stop) | Err(_) => return,
                Ok(_) => {}
            }
        };
        if let Some(prepare) = self.prepare.take() {
            prepare();
        }
        let conn = match (self.connect)() {
            Ok(conn) => conn,
            Err(e) => {
                log::warn!("Global shortcuts portal failed: {e}");
                self.shared.set_state(State::Unavailable, "no session bus");
                return;
            }
        };
        if let Err(Stopped) = self.serve(&conn, first) {
            log::debug!("Global shortcuts portal stopped");
        }
        self.close_session(&conn);
    }

    fn call(&self, conn: &Connection, dest: &str, path: &str, iface: &str, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType))
        -> zbus::Result<zbus::Message> {
        conn.call_method(Some(dest), path, Some(iface), method, body)
    }

    fn serve(&mut self, conn: &Connection, first: Vec<Hotkey>) -> Result<(), Stopped> {
        // First of all, or the portal can't tell which app's shortcuts these are
        if let Err(e) = self.call(conn, BUS_NAME, PORTAL_PATH, REGISTRY, "Register", &(self.app_id.as_str(), HashMap::<&str, Value>::new())) {
            log::info!("Couldn't register with the desktop portal as {}: {e}", self.app_id);
        }
        let version = self.call(conn, BUS_NAME, PORTAL_PATH, "org.freedesktop.DBus.Properties", "Get", &(SHORTCUTS, "version"))
            .and_then(|reply| reply.body().deserialize::<OwnedValue>().map_err(Into::into))
            .ok()
            .and_then(|v| match &*v { Value::U32(n) => Some(*n), Value::Value(inner) => u32::try_from(inner.as_ref()).ok(), _ => None });
        let Some(version) = version else {
            log::info!("The desktop portal has no global shortcuts");
            self.shared.set_state(State::Unavailable, "");
            return Ok(());
        };
        self.shared.inner.lock().unwrap().version = version;
        self.listen(conn);
        self.shared.set_state(State::Ready, "");
        self.bind(conn, first)?;
        loop {
            let mut cmds = match self.cmds.recv() {
                Ok(cmd) => vec![cmd],
                Err(_) => return Ok(()),
            };
            cmds.extend(self.cmds.try_iter());
            if cmds.iter().any(|c| matches!(c, Cmd::Stop)) {
                return Ok(());
            }
            // Only the latest bindings matter
            if let Some(hotkeys) = cmds.iter().rev().find_map(|c| if let Cmd::Bind(h) = c { Some(h.clone()) } else { None }) {
                self.bind(conn, hotkeys)?;
            }
            let session = self.shared.inner.lock().unwrap().session.clone();
            if let (true, Some(session)) = (cmds.iter().any(|c| matches!(c, Cmd::Configure)), session) {
                let Ok(path) = ObjectPath::try_from(session.as_str()) else { continue };
                if let Err(e) = self.call(conn, BUS_NAME, PORTAL_PATH, SHORTCUTS, "ConfigureShortcuts", &(path, "", HashMap::<&str, Value>::new())) {
                    log::info!("Couldn't open the shortcut settings: {e}");
                }
            }
        }
    }

    /// Presses, releases and changed keys for this session, on a thread of their own.
    fn listen(&self, conn: &Connection) {
        let rule = MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(SHORTCUTS).and_then(|r| r.path(PORTAL_PATH)).map(|r| r.build());
        let Ok(rule) = rule else { return };
        let Ok(incoming) = MessageIterator::for_match_rule(rule, conn, Some(64)) else { return };
        let shared = self.shared.clone();
        let _ = std::thread::Builder::new().name("portal-signals".into()).spawn(move || {
            for msg in incoming {
                if shared.stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(msg) = msg else { return };
                let member = msg.header().member().map(|m| m.to_string()).unwrap_or_default();
                let session = shared.inner.lock().unwrap().session.clone();
                let body = msg.body();
                match member.as_str() {
                    "Activated" | "Deactivated" => {
                        let Ok((path, id, _, _)) = body.deserialize::<(ObjectPath, String, u64, HashMap<String, OwnedValue>)>() else { continue };
                        if session.as_deref() != Some(path.as_str()) {
                            continue;
                        }
                        if member == "Activated" { shared.signals.pressed(&id) } else { shared.signals.released(&id) }
                    }
                    "ShortcutsChanged" => {
                        let Ok((path, changed)) = body.deserialize::<(ObjectPath, Vec<(String, HashMap<String, OwnedValue>)>)>() else { continue };
                        if session.as_deref() != Some(path.as_str()) {
                            continue;
                        }
                        {
                            let mut i = shared.inner.lock().unwrap();
                            for (id, props) in &changed {
                                i.statuses.insert(id.clone(), status_of(Some(props)));
                            }
                        }
                        shared.signals.changed();
                    }
                    _ => {}
                }
            }
        });
    }

    fn bind(&self, conn: &Connection, hotkeys: Vec<Hotkey>) -> Result<(), Stopped> {
        self.close_session(conn);
        if hotkeys.is_empty() {
            self.shared.inner.lock().unwrap().statuses.clear();
            self.shared.set_state(State::Ready, "");
            self.forget_unbound(conn);
            return Ok(());
        }
        let session_token = format!("telescope{}", token(8));
        let (code, results) = self.request(conn, "CreateSession", |token| {
            let options: HashMap<&str, Value> = HashMap::from([("handle_token", Value::from(token)), ("session_handle_token", Value::from(session_token.as_str()))]);
            self.call(conn, BUS_NAME, PORTAL_PATH, SHORTCUTS, "CreateSession", &(options,))
        })?;
        let session = results.get("session_handle").and_then(|v| text(v));
        let (true, Some(session)) = (code == 0, session) else {
            log::info!("The desktop portal didn't make a shortcuts session ({code})");
            self.shared.set_state(State::Unavailable, "no session");
            return Ok(());
        };
        {
            let mut i = self.shared.inner.lock().unwrap();
            i.session = Some(session.clone());
            i.state = State::Asking;
        }
        self.shared.signals.changed();
        let Ok(path) = ObjectPath::try_from(session.as_str()) else { return Ok(()) };
        let shortcuts: Vec<(String, HashMap<&str, Value>)> = hotkeys.iter().map(|hk| {
            let mut props: HashMap<&str, Value> = HashMap::from([("description", Value::from(hk.description.clone()))]);
            let trigger = keys::to_trigger(&hk.keys);
            if !trigger.is_empty() {
                props.insert("preferred_trigger", Value::from(trigger));
            }
            (hk.id.clone(), props)
        }).collect();
        let (code, results) = self.request(conn, "BindShortcuts", |token| {
            let options: HashMap<&str, Value> = HashMap::from([("handle_token", Value::from(token))]);
            self.call(conn, BUS_NAME, PORTAL_PATH, SHORTCUTS, "BindShortcuts", &(&path, &shortcuts, "", options))
        })?;
        let bound: HashMap<String, HashMap<String, OwnedValue>> =
            if code == 0 { results.get("shortcuts").map(|v| shortcut_list(v)).unwrap_or_default().into_iter().collect() } else { HashMap::new() };
        {
            let mut i = self.shared.inner.lock().unwrap();
            i.statuses = hotkeys.iter().map(|hk| (hk.id.clone(), status_of(bound.get(&hk.id)))).collect();
            i.state = State::Ready;
        }
        self.shared.signals.changed();
        if code == 0 {
            self.forget_unbound(conn);
        }
        Ok(())
    }

    /// Plasma lists every shortcut a session ever bound, greyed out once none does: drop the ones no session has now.
    fn forget_unbound(&self, conn: &Connection) {
        let component = self.call(conn, KGLOBALACCEL, "/kglobalaccel", "org.kde.KGlobalAccel", "getComponent", &(self.app_id.as_str(),))
            .and_then(|reply| reply.body().deserialize::<zbus::zvariant::OwnedObjectPath>().map_err(Into::into));
        let done = component.and_then(|path| self.call(conn, KGLOBALACCEL, path.as_str(), "org.kde.kglobalaccel.Component", "cleanUp", &()));
        if let Err(e) = done {
            // Not Plasma, or nothing stored under this id yet
            log::debug!("No Plasma shortcuts to tidy for {}: {e}", self.app_id);
        }
    }

    /// Calls a portal method that answers through a Request object's Response signal: (code, results).
    fn request(&self, conn: &Connection, method: &str, send: impl FnOnce(&str) -> zbus::Result<zbus::Message>)
        -> Result<(u32, HashMap<String, OwnedValue>), Stopped> {
        let token = format!("telescope{}", token(12));
        let sender = conn.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
        let handle = format!("{PORTAL_PATH}/request/{sender}/{token}");
        let rule = MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(REQUEST)
            .and_then(|r| r.member("Response")).and_then(|r| r.path(handle.as_str())).map(|r| r.build());
        let responses = match rule.and_then(|rule| MessageIterator::for_match_rule(rule, conn, Some(4))) {
            Ok(it) => it,
            Err(e) => {
                log::info!("Desktop portal {method} failed: {e}");
                return Ok((2, HashMap::new()));
            }
        };
        if let Err(e) = send(&token) {
            log::info!("Desktop portal {method} failed: {e}");
            return Ok((2, HashMap::new()));
        }
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new().name("portal-response".into()).spawn(move || {
            for msg in responses.flatten() {
                if let Ok(answer) = msg.body().deserialize::<(u32, HashMap<String, OwnedValue>)>() {
                    let _ = tx.send(answer);
                    return;
                }
            }
        });
        // A dialog waits on the user, for as long as they take
        loop {
            match rx.recv_timeout(WAIT_STEP) {
                Ok(answer) => return Ok(answer),
                Err(RecvTimeoutError::Timeout) if !self.shared.stopping.load(Ordering::SeqCst) => {}
                Err(_) => return Err(Stopped),
            }
        }
    }

    fn close_session(&self, conn: &Connection) {
        let session = self.shared.inner.lock().unwrap().session.take();
        if let Some(session) = session {
            // Already gone is fine
            let _ = self.call(conn, BUS_NAME, &session, SESSION, "Close", &());
        }
    }
}

fn token(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    format!("{:016x}", h.finish())[..len.min(16)].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as Json;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;
    use telescope_core::host::Poster;
    use zbus::message::Type;
    use zbus::names::BusName;

    struct Collect(Arc<Mutex<Vec<(String, Json)>>>);

    impl Poster for Collect {
        fn post(&self, token: &str, data: Json) {
            self.0.lock().unwrap().push((token.into(), data));
        }
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        cond()
    }

    /// A private session bus, gone when dropped.
    struct Bus(Child, String);

    impl Bus {
        fn start() -> Option<Bus> {
            let mut child = Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address"])
                .stdout(Stdio::piped()).spawn().ok()?;
            let mut address = String::new();
            BufReader::new(child.stdout.take()?).read_line(&mut address).ok()?;
            Some(Bus(child, address.trim().to_string()))
        }

        fn connect(&self) -> zbus::Result<Connection> {
            zbus::blocking::connection::Builder::address(self.1.as_str())?.build()
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[derive(Default)]
    struct Portal {
        /// (member, what it carried that the tests look at)
        calls: Vec<(String, String)>,
        session: String,
        accept: bool,
        version: Option<u32>,
        triggers: HashMap<String, String>,
    }

    /// Just enough of xdg-desktop-portal's GlobalShortcuts, and Plasma's store when plasma, to drive PortalHotkeys.
    struct FakePortal {
        conn: Connection,
        state: Arc<Mutex<Portal>>,
    }

    impl FakePortal {
        fn start(bus: &Bus, version: Option<u32>, accept: bool, plasma: bool, triggers: &[(&str, &str)]) -> FakePortal {
            let conn = bus.connect().unwrap();
            conn.request_name(BUS_NAME).unwrap();
            if plasma {
                conn.request_name(KGLOBALACCEL).unwrap();
            }
            let state = Arc::new(Mutex::new(Portal {
                accept, version, triggers: triggers.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), ..Default::default()
            }));
            let (c, st) = (conn.clone(), state.clone());
            std::thread::spawn(move || {
                for msg in MessageIterator::from(&c).flatten() {
                    if msg.message_type() == Type::MethodCall {
                        serve(&c, &st, &msg);
                    }
                }
            });
            FakePortal { conn, state }
        }

        fn calls(&self) -> Vec<String> {
            self.state.lock().unwrap().calls.iter().map(|(m, _)| m.clone()).collect()
        }

        fn emit(&self, member: &str, id: &str) {
            let session = self.state.lock().unwrap().session.clone();
            let path = ObjectPath::try_from(session.as_str()).unwrap();
            self.conn.emit_signal(None::<BusName>, PORTAL_PATH, SHORTCUTS, member, &(path, id, 0u64, HashMap::<&str, Value>::new())).unwrap();
        }

        fn change(&self, id: &str, trigger: &str) {
            let session = self.state.lock().unwrap().session.clone();
            let path = ObjectPath::try_from(session.as_str()).unwrap();
            let props: HashMap<&str, Value> = HashMap::from([("description", Value::from("x")), ("trigger_description", Value::from(trigger))]);
            self.conn.emit_signal(None::<BusName>, PORTAL_PATH, SHORTCUTS, "ShortcutsChanged", &(path, vec![(id, props)])).unwrap();
        }
    }

    fn reply(conn: &Connection, msg: &zbus::Message, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) {
        let _ = conn.send(&zbus::Message::method_return(&msg.header()).unwrap().build(body).unwrap());
    }

    fn serve(conn: &Connection, state: &Mutex<Portal>, msg: &zbus::Message) {
        let header = msg.header();
        let member = header.member().map(|m| m.to_string()).unwrap_or_default();
        let path = header.path().map(|p| p.to_string()).unwrap_or_default();
        let sender = header.sender().map(|s| s.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
        let respond = |handle: &str, code: u32, results: HashMap<&str, Value>| {
            let _ = conn.emit_signal(None::<BusName>, handle, REQUEST, "Response", &(code, results));
        };
        let mut st = state.lock().unwrap();
        st.calls.push((member.clone(), String::new()));
        let body = msg.body();
        match member.as_str() {
            "Register" => {
                let (id, _): (String, HashMap<String, OwnedValue>) = body.deserialize().unwrap();
                st.calls.last_mut().unwrap().1 = id;
                reply(conn, msg, &());
            }
            "Get" => match st.version {
                Some(v) => reply(conn, msg, &Value::from(v)),
                None => {
                    let _ = conn.send(&zbus::Message::error(&header, "org.freedesktop.DBus.Error.UnknownInterface").unwrap().build(&()).unwrap());
                }
            },
            "CreateSession" => {
                let (opts,): (HashMap<String, OwnedValue>,) = body.deserialize().unwrap();
                let handle = format!("{PORTAL_PATH}/request/{sender}/{}", text(&opts["handle_token"]).unwrap());
                st.session = format!("{PORTAL_PATH}/session/{sender}/{}", text(&opts["session_handle_token"]).unwrap());
                let session = st.session.clone();
                drop(st);
                reply(conn, msg, &ObjectPath::try_from(handle.as_str()).unwrap());
                respond(&handle, 0, HashMap::from([("session_handle", Value::from(session))]));
            }
            "BindShortcuts" => {
                let (_, shortcuts, _, opts): (ObjectPath, Vec<(String, HashMap<String, OwnedValue>)>, String, HashMap<String, OwnedValue>) =
                    body.deserialize().unwrap();
                let handle = format!("{PORTAL_PATH}/request/{sender}/{}", text(&opts["handle_token"]).unwrap());
                let first = shortcuts.first().map(|(id, p)| format!("{id} {} {}", text(&p["description"]).unwrap_or_default(),
                                                                    p.get("preferred_trigger").and_then(|v| text(v)).unwrap_or_default()));
                st.calls.last_mut().unwrap().1 = first.unwrap_or_default();
                let accept = st.accept;
                let bound: Vec<(String, HashMap<&str, Value>)> = shortcuts.iter().map(|(id, p)| {
                    let preferred = p.get("preferred_trigger").and_then(|v| text(v)).unwrap_or_default();
                    let trigger = st.triggers.get(id).cloned().unwrap_or(preferred);
                    (id.clone(), HashMap::from([("description", Value::from("x")), ("trigger_description", Value::from(trigger))]))
                }).collect();
                drop(st);
                reply(conn, msg, &ObjectPath::try_from(handle.as_str()).unwrap());
                if accept {
                    respond(&handle, 0, HashMap::from([("shortcuts", Value::from(bound))]));
                } else {
                    respond(&handle, 1, HashMap::new());
                }
            }
            "ConfigureShortcuts" | "Close" => reply(conn, msg, &()),
            "getComponent" => {
                let (id,): (String,) = body.deserialize().unwrap();
                reply(conn, msg, &ObjectPath::try_from(format!("/component/{id}")).unwrap());
            }
            "cleanUp" => {
                st.calls.last_mut().unwrap().1 = path;
                reply(conn, msg, &true);
            }
            _ => {
                let _ = conn.send(&zbus::Message::error(&header, "org.freedesktop.DBus.Error.UnknownMethod").unwrap().build(&()).unwrap());
            }
        }
    }

    struct Setup {
        portal: FakePortal,
        backend: PortalHotkeys,
        prepared: Arc<Mutex<bool>>,
        posted: Arc<Mutex<Vec<(String, Json)>>>,
        _bus: Bus,
    }

    fn setup(version: Option<u32>, accept: bool, plasma: bool, triggers: &[(&str, &str)]) -> Option<Setup> {
        let Some(bus) = Bus::start() else {
            eprintln!("needs dbus-daemon");
            return None;
        };
        let portal = FakePortal::start(&bus, version, accept, plasma, triggers);
        let posted = Arc::new(Mutex::new(Vec::new()));
        let prepared = Arc::new(Mutex::new(false));
        let p = prepared.clone();
        let address = bus.1.clone();
        let connect: Connect = Box::new(move || zbus::blocking::connection::Builder::address(address.as_str())?.build());
        let backend = PortalHotkeys::new("telescope", Signals(Arc::new(Collect(posted.clone()))), connect,
                                         Some(Box::new(move || *p.lock().unwrap() = true)));
        Some(Setup { portal, backend, prepared, posted, _bus: bus })
    }

    fn hk(id: &str, keys: &str, description: &str) -> Hotkey {
        Hotkey { id: id.into(), keys: keys.into(), description: description.into() }
    }

    fn working(b: &PortalHotkeys, id: &str) -> bool {
        b.status(id).is_some_and(|s| s.working)
    }

    fn cleaned(p: &FakePortal) -> bool {
        p.state.lock().unwrap().calls.iter().any(|(m, d)| m == "cleanUp" && d == "/component/telescope")
    }

    #[test]
    fn binds_and_relays_presses() {
        let Some(mut s) = setup(Some(2), true, true, &[("b2", "Meta+T")]) else { return };
        assert!(!*s.prepared.lock().unwrap(), "nothing happens before there's something to bind");
        s.backend.apply(&[hk("b1", "Ctrl+Alt+M", "Mute: switch on or off"), hk("b2", "Ctrl+T", "Torch")]);
        assert!(wait_for(|| working(&s.backend, "b1")));
        assert!(*s.prepared.lock().unwrap());
        assert_eq!(s.portal.calls()[..4], ["Register", "Get", "CreateSession", "BindShortcuts"]);
        let calls = s.portal.state.lock().unwrap().calls.clone();
        assert_eq!(calls[0].1, "telescope", "first of all, as the portal requires");
        assert_eq!(calls[3].1, "b1 Mute: switch on or off CTRL+ALT+m");
        assert_eq!(s.backend.status("b1").unwrap().text, "Everywhere: CTRL+ALT+m");
        // The desktop picked another key
        assert_eq!(s.backend.status("b2").unwrap().text, "Everywhere: Meta+T");
        assert!(s.backend.can_configure());

        s.portal.emit("Activated", "b1");
        s.portal.emit("Deactivated", "b1");
        let presses = |posted: &Mutex<Vec<(String, Json)>>| posted.lock().unwrap().iter()
            .filter(|(_, d)| d["kind"] != "changed").map(|(_, d)| format!("{} {}", d["kind"].as_str().unwrap(), d["id"].as_str().unwrap()))
            .collect::<Vec<_>>();
        assert!(wait_for(|| presses(&s.posted) == ["pressed b1", "released b1"]));

        s.portal.change("b2", "");
        assert!(wait_for(|| !working(&s.backend, "b2")));
        assert!(s.backend.status("b2").unwrap().text.contains("No key set"));

        assert!(s.backend.configure());
        assert!(wait_for(|| s.portal.calls().contains(&"ConfigureShortcuts".to_string())));

        // New bindings mean a new session: the old one is closed first
        let old = s.portal.state.lock().unwrap().session.clone();
        s.backend.apply(&[hk("b3", "Ctrl+F1", "x")]);
        assert!(wait_for(|| working(&s.backend, "b3")));
        assert_ne!(s.portal.state.lock().unwrap().session, old);
        assert!(s.portal.calls().contains(&"Close".to_string()));
        s.portal.emit("Activated", "b3");
        assert!(wait_for(|| presses(&s.posted).contains(&"pressed b3".to_string())));
        s.backend.stop();
    }

    #[test]
    fn has_plasma_forget_shortcuts_no_session_binds_any_more() {
        let Some(mut s) = setup(Some(2), true, true, &[]) else { return };
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute"), hk("b2", "Ctrl+T", "Torch")]);
        assert!(wait_for(|| cleaned(&s.portal)));
        s.portal.state.lock().unwrap().calls.clear();
        // b2 removed: Plasma would keep listing it
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute")]);
        assert!(wait_for(|| cleaned(&s.portal)));
        let calls = s.portal.calls();
        let at = |m: &str| calls.iter().position(|c| c == m).unwrap();
        assert!(at("BindShortcuts") < at("cleanUp"));
        s.portal.state.lock().unwrap().calls.clear();
        // The last one removed: its session closes, and nothing binds again
        s.backend.apply(&[]);
        assert!(wait_for(|| cleaned(&s.portal)));
        s.backend.stop();
    }

    #[test]
    fn tidies_plasma_without_a_session_when_nothing_is_global() {
        let Some(mut s) = setup(Some(2), true, true, &[]) else { return };
        s.backend.apply(&[]);
        assert!(wait_for(|| cleaned(&s.portal)));
        assert!(!*s.prepared.lock().unwrap());
        assert!(!s.portal.calls().iter().any(|c| c == "Register" || c == "CreateSession"));
        s.backend.stop();
    }

    #[test]
    fn keeps_shortcuts_the_user_turned_down_and_works_without_plasma() {
        let Some(mut s) = setup(Some(2), false, false, &[]) else { return };
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute")]);
        assert!(wait_for(|| s.backend.status("b1").is_some_and(|st| st.text.contains("Not confirmed"))));
        // Declined: nothing to tidy
        assert!(!s.portal.calls().iter().any(|c| c == "getComponent" || c == "cleanUp"));
        s.portal.state.lock().unwrap().accept = true;
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute")]);
        // No kglobalaccel on this bus: binding still works
        assert!(wait_for(|| working(&s.backend, "b1")));
        s.backend.stop();
    }

    #[test]
    fn without_global_shortcuts_says_so() {
        let Some(mut s) = setup(None, true, true, &[]) else { return };
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute")]);
        assert!(wait_for(|| s.backend.status("b1").is_some()));
        assert!(!working(&s.backend, "b1"));
        assert!(s.backend.note().contains("doesn't offer global shortcuts"));
        assert!(!s.backend.can_configure());
        assert!(!s.backend.can_rebind());
        s.backend.stop();
    }

    #[test]
    fn shortcuts_the_user_turned_down() {
        let Some(mut s) = setup(Some(2), false, true, &[]) else { return };
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute")]);
        assert!(wait_for(|| s.backend.status("b1").is_some_and(|st| st.text.contains("Not confirmed"))));
        assert!(s.backend.can_rebind());
        s.portal.state.lock().unwrap().accept = true;
        s.backend.apply(&[hk("b1", "Ctrl+M", "Mute")]);
        assert!(wait_for(|| working(&s.backend, "b1")));
        assert!(!s.backend.can_rebind());
        s.backend.stop();
    }
}
