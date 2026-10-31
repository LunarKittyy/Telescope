//! One Telescope per user (telescope.single_instance): the lock later launches find, raising the running window, and
//! `telescope --action` handing an action to the running copy. The socket, the messages and the control key are
//! the Python app's, so either one's `--action` reaches the other.

use telescope_core::i18n::Fields;
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use telescope_core::i18n::tr;

/// Windows: the single-instance port (update_guard.INSTANCE_PORT); Linux uses a per-user abstract Unix socket.
pub const INSTANCE_PORT: u16 = 47823;
const ACTION: &[u8] = b"action:";
const RAISE: &[u8] = b"raise:";
const MAX_REQUEST: usize = 16384;

/// Where the lock lives (update_guard.instance_address).
#[derive(Clone, Debug, PartialEq)]
pub enum Address {
    /// An abstract Unix socket name, without the leading NUL.
    Abstract(String),
    Port(u16),
}

impl Address {
    /// The real app's, or a dev profile's own so it runs next to it.
    pub fn for_profile(dev: bool) -> Address {
        if cfg!(windows) {
            Address::Port(INSTANCE_PORT + u16::from(dev))
        } else {
            Address::Abstract(format!("telescope-{}{}", uid(), if dev { "-dev" } else { "" }))
        }
    }

    /// This copy's. A dev profile gets one per folder (Python has one for every dev profile): telescope-rs keeps its
    /// settings in a profile of its own until it replaces the Python app, and two of those, or one next to the
    /// Python dev desktop, mustn't take each other for the running copy.
    pub fn current() -> Address {
        let Some(folder) = telescope_core::config::dev_profile() else { return Address::for_profile(false) };
        let digest = ring::digest::digest(&ring::digest::SHA256, folder.to_string_lossy().as_bytes());
        let tag = u16::from_be_bytes([digest.as_ref()[0], digest.as_ref()[1]]);
        match Address::for_profile(true) {
            Address::Abstract(name) => Address::Abstract(format!("{name}-{}", &hex(digest.as_ref())[..8])),
            // Above the real app's port and the Python dev profile's
            Address::Port(port) => Address::Port(port + 2 + tag % 1000),
        }
    }
}

/// Whether this is the only copy using a name every copy on this kind of profile shares (the virtual mic's source), and if so keeps it that way for as
/// long as this process runs: profiles each have a lock of their own, while Python's dev profiles all share one, which counts too.
pub fn sole_user_of(name: &str) -> bool {
    let shared = telescope_core::config::dev_profile().is_some().then(|| Address::for_profile(true));
    sole_user_at(&Address::Abstract(format!("telescope-{}-shared-{name}", uid())), shared.as_ref())
}

fn sole_user_at(claim: &Address, python: Option<&Address>) -> bool {
    if python.is_some_and(held_elsewhere) {
        return false;
    }
    match Lock::bind(claim) {
        Ok(lock) => {
            std::mem::forget(lock);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and can't fail
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

/// Who is running this copy, so a shared port (Windows) never raises another user's window.
pub fn user_token() -> String {
    // An empty variable counts as unset, as Python's `or` does
    let set = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let user = set("USERNAME").or_else(|| set("USER")).unwrap_or_else(|| {
        if cfg!(unix) { uid().to_string() } else { String::new() }
    });
    token_of(&user, &windows_session())
}

/// The first 16 hex digits of sha256("user\0session").
fn token_of(user: &str, session: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("{user}\0{session}").as_bytes());
    hex(digest.as_ref())[..16].to_string()
}

#[cfg(windows)]
fn windows_session() -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
    }
    let mut session = 0u32;
    // SAFETY: session points at a live u32 for the call
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } != 0 { session.to_string() } else { String::new() }
}

#[cfg(not(windows))]
fn windows_session() -> String {
    String::new()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A connection to or from the lock.
pub enum Conn {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    Tcp(std::net::TcpStream),
}

impl Conn {
    fn set_timeout(&self, t: Option<Duration>) {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => {
                let _ = s.set_read_timeout(t);
                let _ = s.set_write_timeout(t);
            }
            Conn::Tcp(s) => {
                let _ = s.set_read_timeout(t);
                let _ = s.set_write_timeout(t);
            }
        }
    }

    fn shutdown_write(&self) {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => drop(s.shutdown(std::net::Shutdown::Write)),
            Conn::Tcp(s) => drop(s.shutdown(std::net::Shutdown::Write)),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.read(buf),
            Conn::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Conn::Unix(s) => s.write(buf),
            Conn::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn connect(address: &Address, timeout: Duration) -> std::io::Result<Conn> {
    let conn = match address {
        #[cfg(target_os = "linux")]
        Address::Abstract(name) => {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
            Conn::Unix(std::os::unix::net::UnixStream::connect_addr(&addr)?)
        }
        #[cfg(not(target_os = "linux"))]
        Address::Abstract(_) => return Err(ErrorKind::Unsupported.into()),
        Address::Port(port) => Conn::Tcp(std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], *port).into(), timeout)?),
    };
    conn.set_timeout(Some(timeout));
    Ok(conn)
}

/// The held lock: dropping it lets the next copy take it.
pub enum Lock {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
    Tcp(std::net::TcpListener),
}

impl Lock {
    fn bind(address: &Address) -> std::io::Result<Lock> {
        match address {
            #[cfg(target_os = "linux")]
            Address::Abstract(name) => {
                use std::os::linux::net::SocketAddrExt;
                let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
                Ok(Lock::Unix(std::os::unix::net::UnixListener::bind_addr(&addr)?))
            }
            #[cfg(not(target_os = "linux"))]
            Address::Abstract(_) => Err(ErrorKind::Unsupported.into()),
            // std doesn't set SO_REUSEADDR on Windows, so two copies can't share the port
            Address::Port(port) => Ok(Lock::Tcp(std::net::TcpListener::bind(("127.0.0.1", *port))?)),
        }
    }

    fn accept(&self) -> std::io::Result<Conn> {
        match self {
            #[cfg(unix)]
            Lock::Unix(l) => l.accept().map(|(s, _)| Conn::Unix(s)),
            Lock::Tcp(l) => l.accept().map(|(s, _)| Conn::Tcp(s)),
        }
    }
}

/// Whether something holds the lock at address now (the guard's another_copy_running), letting go of it at once.
pub fn held_elsewhere(address: &Address) -> bool {
    Lock::bind(address).is_err()
}

/// What a start found (acquire_single_instance).
pub enum Acquired {
    /// This copy holds the lock.
    Lock(Lock),
    /// The running copy was asked to show itself.
    Raised,
    /// Another user's Telescope holds it (a shared port on Windows).
    OtherUser,
    /// Something that isn't a Telescope answering holds it, or one that's stuck.
    Taken,
}

/// The lock, or what happened asking the copy that has it to show itself. wait: keep trying this long first, for a
/// relaunch after an update while the old copy exits.
pub fn acquire(address: &Address, wait: Duration) -> Acquired {
    let deadline = Instant::now() + wait;
    loop {
        match Lock::bind(address) {
            Ok(lock) => return Acquired::Lock(lock),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(250)),
            Err(_) => break,
        }
    }
    let mut reply = [0u8; 5];
    let mut got = 0;
    if let Ok(mut c) = connect(address, Duration::from_secs(2)) {
        let mut msg = RAISE.to_vec();
        msg.extend_from_slice(user_token().as_bytes());
        if c.write_all(&msg).is_ok() {
            while got < reply.len() {
                match c.read(&mut reply[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
        }
    }
    match &reply[..got] {
        b"ok" => Acquired::Raised,
        b"other" => Acquired::OtherUser,
        _ => Acquired::Taken,
    }
}

/// What to tell someone whose start didn't get the lock (_other_user_notice, _port_taken_notice): (title, text).
pub fn notice(acquired: &Acquired, address: &Address) -> Option<(String, String)> {
    match acquired {
        Acquired::OtherUser => Some((tr("Telescope is already running"),
                                     tr("Telescope is already running for another user on this computer. Close it there, then open it again here."))),
        Acquired::Taken => {
            let place = match address {
                Address::Port(port) => tr("port {port}").field("port", port.to_string()),
                Address::Abstract(_) => tr("a local socket"),
            };
            Some((tr("Telescope can't start"),
                  tr("Telescope couldn't start because something else is holding its single-instance lock ({where}), or another Telescope is stuck. Close that program, or restart the computer, then open Telescope again.")
                      .field("where", &place)))
        }
        _ => None,
    }
}

/// Where the running copy keeps the key `--action` has to send: next to the settings, so another account on the
/// computer (which can reach the socket too) can't drive this one's controls.
pub fn control_key_path(config_file: &Path) -> PathBuf {
    config_file.parent().unwrap_or(Path::new(".")).join(".control-key")
}

/// A new key for this run, readable only by this user; None if it couldn't be written (then --action can't work).
pub fn write_control_key(path: &Path) -> Option<String> {
    let mut raw = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw).ok()?;
    let key = hex(ring::digest::digest(&ring::digest::SHA256, &raw).as_ref());
    let written = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A new file gets the mode below; an old one would keep its own
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)?.write_all(key.as_bytes())
    })();
    match written {
        Ok(()) => Some(key),
        Err(e) => {
            log::error!("Couldn't write the key for telescope --action: {e}");
            None
        }
    }
}

/// For `telescope --action`: hands request ({"action", "params"} or {"list": true}) to the running copy. Returns
/// (ok, text to print).
pub fn send_action(address: &Address, key_path: &Path, request: &Value) -> (bool, String) {
    let not_running = || (false, tr("Telescope isn't running."));
    let Ok(key) = std::fs::read(key_path) else { return not_running() };
    let mut body = request.clone();
    body["key"] = json!(String::from_utf8_lossy(&key).trim());
    let mut payload = ACTION.to_vec();
    payload.extend_from_slice(body.to_string().as_bytes());
    let Ok(mut c) = connect(address, Duration::from_secs(8)) else { return not_running() };
    if c.write_all(&payload).is_err() {
        return not_running();
    }
    c.shutdown_write();
    let mut reply = Vec::new();
    let mut chunk = [0u8; 65536];
    while reply.len() < 1 << 20 {
        match c.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&chunk[..n]),
            Err(_) => return not_running(),
        }
    }
    match serde_json::from_slice::<Value>(&reply) {
        Ok(answer) if answer.get("ok").and_then(Value::as_bool).is_some() && answer.get("text").is_some() => {
            let text = answer["text"].as_str().map_or_else(|| answer["text"].to_string(), str::to_string);
            (answer["ok"].as_bool().unwrap_or(false), text)
        }
        _ => (false, tr("Telescope didn't understand that. Is it an older version? Update it to use --action.")),
    }
}

/// hmac.compare_digest: equal, taking as long whatever the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn read_request(conn: &mut Conn, first: &[u8]) -> Option<Value> {
    let mut data = first.to_vec();
    // A client that trickles bytes mustn't hold up every later raise
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut chunk = [0u8; 4096];
    while data.len() < MAX_REQUEST {
        if Instant::now() > deadline {
            return None;
        }
        match conn.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(_) => return None,
        }
    }
    serde_json::from_slice::<Value>(&data[ACTION.len()..]).ok().filter(Value::is_object)
}

/// What the lock answers one connection with (listen_for_raise's body): raise:<user token> brings the window up;
/// action:<json> carrying control_key runs on_action and sends back its {"ok", "text"}.
pub fn answer(conn: &mut Conn, on_raise: &dyn Fn(), on_action: Option<&dyn Fn(Value) -> Value>, control_key: Option<&str>) {
    conn.set_timeout(Some(Duration::from_secs(1)));
    let mut first = [0u8; 4096];
    let n = match conn.read(&mut first) {
        Ok(n) => n,
        Err(_) => return,
    };
    let message = &first[..n];
    let reply: Vec<u8> = if message.starts_with(ACTION) {
        let request = read_request(conn, message);
        let sent = request.as_ref().and_then(|r| r.get("key")).and_then(Value::as_str).unwrap_or("").to_string();
        let answer = match (request, on_action, control_key) {
            (Some(_), Some(_), None) => json!({"ok": false, "text": tr("Telescope couldn't write the key --action needs next to its settings when it started. Restart it.")}),
            (Some(mut request), Some(run), Some(key)) if same(sent.as_bytes(), key.as_bytes()) => {
                request.as_object_mut().map(|o| o.remove("key"));
                run(request)
            }
            _ => json!({"ok": false, "text": tr("Telescope turned that down: it didn't come from this user's Telescope command.")}),
        };
        answer.to_string().into_bytes()
    } else if message.starts_with(RAISE) && &message[RAISE.len()..] == user_token().as_bytes() {
        on_raise();
        // Tells the new copy a Telescope answered, not some other program holding the lock
        b"ok".to_vec()
    } else if message.starts_with(RAISE) {
        // Another user's Telescope, on a port both users can reach
        b"other".to_vec()
    } else {
        return;
    };
    let _ = conn.write_all(&reply);
}

/// Answers later launches on a thread of its own until the lock is dropped with the process.
pub fn listen(lock: Lock, on_raise: impl Fn() + Send + 'static, on_action: impl Fn(Value) -> Value + Send + 'static,
              control_key: Option<String>) {
    std::thread::Builder::new().name("single-instance".into()).spawn(move || loop {
        match lock.accept() {
            Ok(mut conn) => answer(&mut conn, &on_raise, Some(&on_action), control_key.as_deref()),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }).expect("couldn't start the single-instance thread");
}

/// --action's words as a request: the id, then name=value settings (action_request).
pub fn action_request(words: &[String]) -> Result<Value, String> {
    let (id, rest) = words.split_first().ok_or("--action needs an action id")?;
    let mut params = serde_json::Map::new();
    for word in rest {
        match word.split_once('=') {
            Some((name, value)) if !name.is_empty() => {
                params.insert(name.into(), json!(value));
            }
            _ => return Err(format!("'{word}' isn't NAME=VALUE")),
        }
    }
    Ok(json!({"action": id, "params": params}))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A mic source goes by one name on every dev profile, and each Rust profile has its own lock
    #[cfg(target_os = "linux")]
    #[test]
    fn a_shared_name_has_one_user_at_a_time() {
        let name = |what: &str| Address::Abstract(format!("telescope-test-{}-{what}", std::process::id()));
        let (claim, python) = (name("claim"), name("python"));
        let running = Lock::bind(&python).unwrap();
        assert!(!sole_user_at(&claim, Some(&python)), "a Python dev desktop is running");
        drop(running);
        assert!(sole_user_at(&claim, Some(&python)));
        assert!(!sole_user_at(&claim, Some(&python)), "another Rust profile holds it now");
        assert!(sole_user_at(&name("other"), None), "the real app's name has no dev lock to ask about");
    }
    use std::sync::{Arc, Mutex};

    fn test_address(name: &str) -> Address {
        if cfg!(target_os = "linux") {
            Address::Abstract(format!("telescope-test-{}-{name}", std::process::id()))
        } else {
            let digest = ring::digest::digest(&ring::digest::SHA256, format!("{}{name}", std::process::id()).as_bytes());
            Address::Port(47900 + u16::from_be_bytes([digest.as_ref()[0], digest.as_ref()[1]]) % 2000)
        }
    }

    /// A running copy at a test address: how many raises reached it, and the actions, as they arrived.
    struct Running {
        address: Address,
        raised: Arc<Mutex<u32>>,
        actions: Arc<Mutex<Vec<Value>>>,
        dir: PathBuf,
    }

    impl Running {
        fn with_key(name: &str, key: impl FnOnce(&Path) -> Option<String>) -> Running {
            let address = test_address(name);
            let Acquired::Lock(lock) = acquire(&address, Duration::ZERO) else { panic!("no lock") };
            let (raised, actions) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(Vec::new())));
            let dir = std::env::temp_dir().join(format!("telescope-si-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let control_key = key(&control_key_path(&dir.join("telescope_config.json")));
            let (r, a) = (raised.clone(), actions.clone());
            listen(lock, move || *r.lock().unwrap() += 1, move |request| {
                a.lock().unwrap().push(request);
                json!({"ok": true, "text": "Torch: on"})
            }, control_key);
            Running { address, raised, actions, dir }
        }

        fn new(name: &str) -> Running {
            Running::with_key(name, write_control_key)
        }

        fn key_path(&self) -> PathBuf {
            control_key_path(&self.dir.join("telescope_config.json"))
        }

        fn raises(&self) -> u32 {
            *self.raised.lock().unwrap()
        }

        /// What it answers these bytes with, all of it.
        fn says(&self, words: &[u8]) -> Vec<u8> {
            let mut c = connect(&self.address, Duration::from_secs(5)).unwrap();
            c.write_all(words).unwrap();
            let mut reply = Vec::new();
            let _ = c.read_to_end(&mut reply); // read_to_end returning means it closed the connection
            reply
        }

        fn raise(&self) -> Vec<u8> {
            self.says(&[RAISE, user_token().as_bytes()].concat())
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_running_telescope_answers_a_raise_with_ok() {
        let run = Running::new("answers");
        assert_eq!(run.raise(), b"ok");
        assert_eq!(run.raises(), 1);
    }

    #[test]
    fn listen_for_raise_invokes_callback_and_closes_connection() {
        let run = Running::new("closes");
        assert_eq!(run.raise(), b"ok"); // says() only returns once the other end closed
    }

    #[test]
    fn listen_for_raise_ignores_wrong_message_and_timeouts() {
        let run = Running::new("wrong");
        assert_eq!(run.says(b"other"), b"");
        assert_eq!(run.raises(), 0);
        assert_eq!(run.raise(), b"ok", "still listening");
    }

    #[test]
    fn a_silent_connection_does_not_stop_later_raises() {
        // Anything on this machine that connects and says nothing used to block the listener for good
        let run = Running::new("silent-peer");
        let silent = connect(&run.address, Duration::from_secs(5)).unwrap();
        assert_eq!(run.raise(), b"ok");
        drop(silent);
    }

    #[test]
    fn a_reset_connection_does_not_stop_later_raises() {
        let run = Running::new("reset");
        drop(connect(&run.address, Duration::from_secs(5)).unwrap());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(run.raise(), b"ok");
    }

    #[test]
    fn the_listener_only_raises_for_the_same_user() {
        let run = Running::new("other-user");
        assert_eq!(run.says(&[RAISE, b"0000000000000000"].concat()), b"other");
        assert_eq!(run.raises(), 0);
    }

    #[test]
    fn another_users_telescope_on_a_shared_port_answers_other() {
        let address = test_address("shared-port");
        let Acquired::Lock(lock) = acquire(&address, Duration::ZERO) else { panic!("no lock") };
        std::thread::spawn(move || {
            let mut conn = lock.accept().unwrap();
            let _ = conn.read(&mut [0u8; 64]);
            conn.write_all(b"other").unwrap();
        });
        let acquired = acquire(&address, Duration::ZERO);
        assert!(matches!(acquired, Acquired::OtherUser));
        assert!(notice(&acquired, &address).unwrap().1.contains("another user"));
    }

    #[test]
    fn a_linux_lock_is_per_user_and_a_second_copy_of_the_same_user_gets_ok() {
        let run = Running::new("second-copy");
        assert!(matches!(acquire(&run.address, Duration::ZERO), Acquired::Raised));
        assert_eq!(run.raises(), 1);
        assert!(matches!(acquire(&test_address("second-copy-another-user"), Duration::ZERO), Acquired::Lock(_)), "a different user's name binds alongside");
    }

    #[test]
    fn an_action_reaches_the_running_copy_with_its_key() {
        let run = Running::new("action");
        let request = json!({"action": "camera.torch", "params": {"mode": "on"}});
        assert_eq!(send_action(&run.address, &run.key_path(), &request), (true, "Torch: on".into()));
        assert_eq!(*run.actions.lock().unwrap(), [request], "the key isn't passed on");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(run.key_path()).unwrap().permissions()) & 0o077, 0, "nobody else can read it");
    }

    #[test]
    fn an_action_without_the_right_key_is_turned_down() {
        let run = Running::new("action-no-key");
        std::fs::write(run.key_path(), "not-the-key").unwrap();
        let (ok, text) = send_action(&run.address, &run.key_path(), &json!({"action": "camera.torch"}));
        assert!(!ok && text.contains("turned that down"), "{text}");
        assert!(run.actions.lock().unwrap().is_empty());
    }

    #[test]
    fn an_action_with_nothing_running() {
        let dir = std::env::temp_dir().join(format!("telescope-si-idle-{}", std::process::id()));
        assert_eq!(send_action(&test_address("idle"), &dir.join("no-key"), &json!({"list": true})), (false, "Telescope isn't running.".into()));
    }

    #[test]
    fn a_raise_still_works_next_to_actions() {
        let run = Running::new("raise-and-action");
        assert_eq!(send_action(&run.address, &run.key_path(), &json!({"action": "x"})).0, true);
        assert_eq!(run.raise(), b"ok");
    }

    #[test]
    fn an_action_when_the_key_couldnt_be_written() {
        let run = Running::with_key("action-unwritten", |path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "old-run").unwrap();
            None
        });
        let (ok, text) = send_action(&run.address, &run.key_path(), &json!({"action": "x"}));
        assert!(!ok && text.contains("Restart it"), "{text}");
    }

    #[test]
    fn a_second_start_raises_the_first_and_actions_need_the_key() {
        let address = test_address("raise");
        let Acquired::Lock(lock) = acquire(&address, Duration::ZERO) else { panic!("no lock") };
        let raised = Arc::new(Mutex::new(0));
        let r = raised.clone();
        let dir = std::env::temp_dir().join(format!("telescope-si-{}", std::process::id()));
        let key_path = control_key_path(&dir.join("telescope_config.json"));
        let key = write_control_key(&key_path);
        assert_eq!(key.as_ref().map(String::len), Some(64));
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&key_path).unwrap().permissions()) & 0o777, 0o600);
        listen(lock, move || *r.lock().unwrap() += 1, |req| json!({"ok": true, "text": format!("ran {}", req["action"].as_str().unwrap_or(""))}), key);
        assert!(matches!(acquire(&address, Duration::ZERO), Acquired::Raised));
        assert_eq!(*raised.lock().unwrap(), 1);
        let request = action_request(&["camera.torch".into(), "mode=on".into()]).unwrap();
        assert_eq!(request, json!({"action": "camera.torch", "params": {"mode": "on"}}));
        assert_eq!(send_action(&address, &key_path, &request), (true, "ran camera.torch".into()));
        // A key that isn't this run's is turned down
        std::fs::write(&key_path, "0".repeat(64)).unwrap();
        let (ok, text) = send_action(&address, &key_path, &request);
        assert!(!ok && text.contains("turned that down"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(send_action(&address, &key_path, &request), (false, "Telescope isn't running.".into()));
    }

    #[test]
    fn nothing_listening_means_not_running_and_bad_words_are_refused() {
        let address = test_address("none");
        let dir = std::env::temp_dir().join(format!("telescope-si-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join(".control-key");
        std::fs::write(&key_path, "k").unwrap();
        assert_eq!(send_action(&address, &key_path, &json!({"list": true})), (false, "Telescope isn't running.".into()));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(action_request(&["a".into(), "novalue".into()]), Err("'novalue' isn't NAME=VALUE".into()));
        assert_eq!(action_request(&["a".into(), "=x".into()]), Err("'=x' isn't NAME=VALUE".into()));
    }

    #[test]
    fn a_lock_that_answers_nothing_says_so_and_a_relaunch_waits_for_the_old_copy() {
        let address = test_address("silent");
        let Acquired::Lock(lock) = acquire(&address, Duration::ZERO) else { panic!("no lock") };
        // Something holding the lock that isn't a Telescope: it accepts and closes without a word
        let silent = std::thread::spawn(move || {
            if let Ok(conn) = lock.accept() {
                drop(conn);
            }
            std::thread::sleep(Duration::from_millis(300));
        });
        assert!(matches!(acquire(&address, Duration::ZERO), Acquired::Taken));
        assert!(notice(&Acquired::Taken, &address).unwrap().1.contains("single-instance lock"));
        // The old copy lets go within the wait: the new one gets the lock
        assert!(matches!(acquire(&address, Duration::from_secs(5)), Acquired::Lock(_)));
        silent.join().unwrap();
    }

    // What Python's user_token() makes of the same names (sha256 of "user\0session", 16 hex digits), so either app's raise is the other's
    #[test]
    fn the_user_token_is_the_one_python_makes() {
        assert_eq!(token_of("alice", ""), "bfebef88e4b36874");
        assert_eq!(token_of("win", ""), "20ac7ff5c3b32ba5");
        assert_eq!(token_of("bob", ""), "fa73b451489b2b37");
        // A Windows session number goes in after a NUL, as Python's f"{user}\0{session}" has it
        assert_ne!(token_of("alice", "1"), token_of("alice", ""));
    }

    #[test]
    fn the_address_is_per_user_and_apart_for_a_dev_profile() {
        let (real, dev) = (Address::for_profile(false), Address::for_profile(true));
        assert_ne!(real, dev);
        if cfg!(target_os = "linux") {
            assert_eq!(dev, Address::Abstract(format!("telescope-{}-dev", uid())));
        }
        assert_eq!(user_token().len(), 16);
    }
}
