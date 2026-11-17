//! One connection to a PipeWire daemon with its own loop thread, shared by everything that talks to PipeWire, and the
//! registry listing the nodes it can see.

use super::ffi::{self, CoreEvents, Lib, NodeEvents, RegistryEvents, SpaDict, SpaHook};
use super::pod::{ObjectPod, Scalar, FORMAT_VIDEO_SIZE, PARAM_ENUM_FORMAT, PARAM_FORMAT};
use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// How long a connection waits for the daemon's first answer.
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);

/// A node as the registry lists it, with the properties it was announced with.
#[derive(Clone, Debug, Default)]
pub struct Node {
    pub id: u32,
    /// The daemon's serial for it (object.serial), 0 when it didn't say.
    pub serial: u64,
    pub props: Vec<(String, String)>,
    /// The picture size a video node offers, once it has said (a node that offers a range gets its default).
    pub video_size: Option<(u32, u32)>,
}

impl Node {
    pub fn prop(&self, key: &str) -> Option<&str> {
        self.props.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn name(&self) -> &str {
        self.prop("node.name").unwrap_or("")
    }

    pub fn media_class(&self) -> &str {
        self.prop("media.class").unwrap_or("")
    }
}

struct NodeProxy {
    proxy: *mut c_void,
    hook: Box<SpaHook>,
    context: Box<NodeContext>,
}

// SAFETY: only touched on the loop thread, or after it has stopped
unsafe impl Send for NodeProxy {}

struct State {
    nodes: BTreeMap<u32, Node>,
    proxies: BTreeMap<u32, NodeProxy>,
    synced: bool,
}

/// What the loop thread's callbacks reach.
struct Shared {
    lib: &'static Lib,
    state: Mutex<State>,
    changed: Condvar,
    dead: AtomicBool,
    registry: AtomicUsize,
}

struct NodeContext {
    shared: *const Shared,
    id: u32,
}

/// A connection to PipeWire: the default daemon, or the remote a file descriptor leads to.
pub struct Connection {
    lib: &'static Lib,
    thread_loop: *mut c_void,
    context: *mut c_void,
    core: *mut c_void,
    registry: *mut c_void,
    shared: Box<Shared>,
    // boxed so each hook keeps its address, which libpipewire holds on to
    #[allow(clippy::vec_box)]
    hooks: Mutex<Vec<Box<SpaHook>>>,
}

// SAFETY: the raw pointers belong to libpipewire objects that are only used with the loop locked
unsafe impl Send for Connection {}
// SAFETY: as above, and the rest is behind mutexes
unsafe impl Sync for Connection {}

static CORE_EVENTS: CoreEvents = CoreEvents { version: 0, info: std::ptr::null(), done: Some(on_core_done), ping: std::ptr::null(), error: Some(on_core_error), remove_id: std::ptr::null(), bound_id: std::ptr::null(), add_mem: std::ptr::null(), remove_mem: std::ptr::null(), bound_props: std::ptr::null() };
static REGISTRY_EVENTS: RegistryEvents = RegistryEvents { version: 0, global: Some(on_global), global_remove: Some(on_global_remove) };
static NODE_EVENTS: NodeEvents = NodeEvents { version: 0, info: std::ptr::null(), param: Some(on_node_param) };

/// Runs a callback body without letting a panic unwind into C.
fn guarded(body: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
}

unsafe extern "C" fn on_core_done(data: *mut c_void, id: u32, _seq: c_int) {
    guarded(|| {
        if id != 0 {
            return;
        }
        // SAFETY: data is the Shared the listener was added with, which outlives the loop thread
        let shared = unsafe { &*(data as *const Shared) };
        if let Ok(mut state) = shared.state.lock() {
            state.synced = true;
        }
        shared.changed.notify_all();
    });
}

unsafe extern "C" fn on_core_error(data: *mut c_void, id: u32, _seq: c_int, res: c_int, _message: *const c_char) {
    guarded(|| {
        // An error on the core itself (id 0) with a broken pipe is the daemon going away
        if id == 0 && (res == -libc::EPIPE || res == -libc::ECONNRESET) {
            // SAFETY: as on_core_done
            let shared = unsafe { &*(data as *const Shared) };
            shared.dead.store(true, Ordering::SeqCst);
            shared.changed.notify_all();
        }
    });
}

unsafe extern "C" fn on_global(data: *mut c_void, id: u32, _permissions: u32, kind: *const c_char, _version: u32, props: *const SpaDict) {
    guarded(|| {
        // SAFETY: kind is a NUL-terminated string
        if kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_bytes() != ffi::PW_TYPE_NODE.as_bytes() {
            return;
        }
        // SAFETY: as on_core_done
        let shared = unsafe { &*(data as *const Shared) };
        // SAFETY: props is valid for this call
        let props = unsafe { ffi::dict_pairs(props) };
        let serial = props.iter().find(|(k, _)| k == "object.serial").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
        let video = props.iter().any(|(k, v)| k == "media.class" && v.contains("Video"));
        let node = Node { id, serial, props, video_size: None };
        if let Ok(mut state) = shared.state.lock() {
            state.nodes.insert(id, node);
        }
        shared.changed.notify_all();
        if !video {
            return;
        }
        // The listing is unlocked while binding, so nothing that answers right away can wait on it
        let registry = shared.registry.load(Ordering::SeqCst) as *mut c_void;
        // SAFETY: the registry is live while its events arrive
        let proxy = unsafe { ffi::registry_bind_node(registry, id) };
        if proxy.is_null() {
            return;
        }
        let mut context = Box::new(NodeContext { shared, id });
        let mut hook = SpaHook::new();
        // SAFETY: the proxy is live, and the hook and context are kept in the proxies map until it is destroyed
        unsafe { ffi::node_listen(proxy, &mut *hook, &NODE_EVENTS, &mut *context as *mut NodeContext as *mut c_void, vec![PARAM_ENUM_FORMAT, PARAM_FORMAT]) };
        if let Ok(mut state) = shared.state.lock() {
            state.proxies.insert(id, NodeProxy { proxy, hook, context });
        }
    });
}

unsafe extern "C" fn on_global_remove(data: *mut c_void, id: u32) {
    guarded(|| {
        // SAFETY: as on_core_done
        let shared = unsafe { &*(data as *const Shared) };
        let Ok(mut state) = shared.state.lock() else { return };
        state.nodes.remove(&id);
        if let Some(proxy) = state.proxies.remove(&id) {
            // SAFETY: the proxy was bound here and is destroyed once
            unsafe { (shared.lib.proxy_destroy)(proxy.proxy) };
            drop((proxy.hook, proxy.context));
        }
        shared.changed.notify_all();
    });
}

unsafe extern "C" fn on_node_param(data: *mut c_void, _seq: c_int, id: u32, _index: u32, _next: u32, param: *const u8) {
    guarded(|| {
        if id != PARAM_ENUM_FORMAT && id != PARAM_FORMAT {
            return;
        }
        // SAFETY: data is the NodeContext the listener was added with, kept alive until the proxy is destroyed
        let context = unsafe { &*(data as *const NodeContext) };
        // SAFETY: param is valid for this call
        let Some(pod) = (unsafe { ObjectPod::from_ptr(param) }) else { return };
        let Some(Scalar::Rect(width, height)) = pod.prop(FORMAT_VIDEO_SIZE).and_then(|p| p.value.default_value()) else { return };
        // SAFETY: the Shared outlives the loop thread
        let shared = unsafe { &*context.shared };
        let Ok(mut state) = shared.state.lock() else { return };
        if let Some(node) = state.nodes.get_mut(&context.id) {
            node.video_size = Some((width, height));
        }
        shared.changed.notify_all();
    });
}

impl Connection {
    /// The connection everything shares, made on first use and let go of when nobody holds it.
    pub fn shared() -> Result<Arc<Connection>, String> {
        static SHARED: Mutex<Weak<Connection>> = Mutex::new(Weak::new());
        let mut slot = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(connection) = slot.upgrade().filter(|c| !c.is_dead()) {
            return Ok(connection);
        }
        let connection = Connection::connect()?;
        *slot = Arc::downgrade(&connection);
        Ok(connection)
    }

    /// A connection of its own to the default daemon (PIPEWIRE_REMOTE or pipewire-0).
    pub fn connect() -> Result<Arc<Connection>, String> {
        Connection::open(None)
    }

    /// A connection to the remote a file descriptor leads to, such as the one the screen cast portal hands over.
    pub fn connect_fd(fd: OwnedFd) -> Result<Arc<Connection>, String> {
        Connection::open(Some(fd))
    }

    fn open(fd: Option<OwnedFd>) -> Result<Arc<Connection>, String> {
        let lib = ffi::lib()?;
        // SAFETY: the loop is made, started and used through libpipewire's own calls, in the order its documentation gives
        unsafe {
            let thread_loop = (lib.thread_loop_new)(c"telescope-pipewire".as_ptr(), std::ptr::null());
            if thread_loop.is_null() {
                return Err("PipeWire's loop couldn't be made".into());
            }
            (lib.thread_loop_lock)(thread_loop);
            let context = (lib.context_new)((lib.thread_loop_get_loop)(thread_loop), std::ptr::null_mut(), 0);
            let core = if context.is_null() {
                std::ptr::null_mut()
            } else {
                match fd {
                    Some(fd) => (lib.context_connect_fd)(context, fd.into_raw_fd(), std::ptr::null_mut(), 0),
                    None => (lib.context_connect)(context, std::ptr::null_mut(), 0),
                }
            };
            if core.is_null() {
                if !context.is_null() {
                    (lib.context_destroy)(context);
                }
                (lib.thread_loop_unlock)(thread_loop);
                (lib.thread_loop_destroy)(thread_loop);
                return Err("PipeWire isn't running".into());
            }
            let registry = ffi::core_get_registry(core);
            let shared = Box::new(Shared { lib, state: Mutex::new(State { nodes: BTreeMap::new(), proxies: BTreeMap::new(), synced: false }), changed: Condvar::new(), dead: AtomicBool::new(false), registry: AtomicUsize::new(registry as usize) });
            let mut core_hook = SpaHook::new();
            let mut registry_hook = SpaHook::new();
            let data = &*shared as *const Shared as *mut c_void;
            ffi::core_add_listener(core, &mut *core_hook, &CORE_EVENTS, data);
            if !registry.is_null() {
                ffi::registry_add_listener(registry, &mut *registry_hook, &REGISTRY_EVENTS, data);
            }
            ffi::core_sync(core, 1);
            (lib.thread_loop_start)(thread_loop);
            (lib.thread_loop_unlock)(thread_loop);
            let connection = Arc::new(Connection { lib, thread_loop, context, core, registry, shared, hooks: Mutex::new(vec![core_hook, registry_hook]) });
            // The first answer carries every node that was there already
            let state = connection.shared.state.lock().map_err(|_| "PipeWire's state broke".to_string())?;
            let (state, _) = connection.shared.changed.wait_timeout_while(state, HELLO_TIMEOUT, |s| !s.synced && !connection.is_dead()).map_err(|_| "PipeWire's state broke".to_string())?;
            let ready = state.synced;
            drop(state);
            if !ready {
                return Err("PipeWire didn't answer".into());
            }
            Ok(connection)
        }
    }

    /// Whether the daemon has gone away.
    pub fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::SeqCst)
    }

    /// Every node the daemon lists now.
    pub fn nodes(&self) -> Vec<Node> {
        self.shared.state.lock().map(|s| s.nodes.values().cloned().collect()).unwrap_or_default()
    }

    /// One node, if it's still there.
    pub fn node(&self, id: u32) -> Option<Node> {
        self.shared.state.lock().ok().and_then(|s| s.nodes.get(&id).cloned())
    }

    /// Runs `body` with the loop locked, which is what any call into a PipeWire object from another thread needs.
    pub(super) fn locked<R>(&self, body: impl FnOnce() -> R) -> R {
        // SAFETY: the thread loop lives as long as the connection
        unsafe { (self.lib.thread_loop_lock)(self.thread_loop) };
        let result = body();
        // SAFETY: as above, and locked just now
        unsafe { (self.lib.thread_loop_unlock)(self.thread_loop) };
        result
    }

    pub(super) fn lib(&self) -> &'static Lib {
        self.lib
    }

    pub(super) fn core(&self) -> *mut c_void {
        self.core
    }

    /// Waits up to `timeout` until `ready` says so for the nodes listed now (a changed listing wakes it).
    pub fn wait_for_nodes(&self, timeout: Duration, ready: impl Fn(&[Node]) -> bool) -> bool {
        let Ok(state) = self.shared.state.lock() else { return false };
        let result = self.shared.changed.wait_timeout_while(state, timeout, |s| {
            let nodes: Vec<Node> = s.nodes.values().cloned().collect();
            !ready(&nodes) && !self.is_dead()
        });
        match result {
            Ok((state, _)) => {
                let nodes: Vec<Node> = state.nodes.values().cloned().collect();
                ready(&nodes)
            }
            Err(_) => false,
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let lib = self.lib;
        // SAFETY: the loop is stopped before anything it uses goes, and every object is destroyed once, in the reverse order it was made
        unsafe {
            (lib.thread_loop_stop)(self.thread_loop);
            let proxies = std::mem::take(&mut self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).proxies);
            for proxy in proxies.into_values() {
                (lib.proxy_destroy)(proxy.proxy);
            }
            if !self.registry.is_null() {
                (lib.proxy_destroy)(self.registry);
            }
            (lib.core_disconnect)(self.core);
            (lib.context_destroy)(self.context);
            (lib.thread_loop_destroy)(self.thread_loop);
        }
        self.hooks.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}
