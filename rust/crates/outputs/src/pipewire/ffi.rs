//! libpipewire loaded at run time: the program has no link-time dependency on it, so a system without PipeWire still
//! starts (tools/start.sh refuses to run a program whose `ldd` shows a missing library). Only the exported functions
//! are bound; the header's inline helpers (registry, node and core calls) are small and done by hand below. The struct
//! layouts were checked against the PipeWire 1.6 headers with a C probe.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

pub const SPA_ID_INVALID: u32 = u32::MAX;
pub const PW_DIRECTION_INPUT: c_int = 0;
pub const PW_DIRECTION_OUTPUT: c_int = 1;
pub const PW_STREAM_FLAG_AUTOCONNECT: u32 = 1;
pub const PW_STREAM_FLAG_DRIVER: u32 = 8;
pub const PW_STREAM_FLAG_ALLOC_BUFFERS: u32 = 256;
pub const PW_VERSION_REGISTRY: u32 = 3;
pub const PW_VERSION_NODE: u32 = 3;
pub const PW_TYPE_NODE: &str = "PipeWire:Interface:Node";

/// Buffer memory kinds (spa_data_type).
pub const SPA_DATA_MEM_PTR: u32 = 1;
pub const SPA_DATA_MEM_FD: u32 = 2;
pub const SPA_DATA_DMA_BUF: u32 = 3;

/// pw_stream_state, as the callbacks get it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    Error,
    Unconnected,
    Connecting,
    Paused,
    Streaming,
}

impl StreamState {
    pub fn from_raw(raw: c_int) -> StreamState {
        match raw {
            0 => StreamState::Unconnected,
            1 => StreamState::Connecting,
            2 => StreamState::Paused,
            3 => StreamState::Streaming,
            _ => StreamState::Error,
        }
    }
}

/// spa_hook: room for libpipewire to link a listener into its list, which must stay put until it is removed.
#[repr(C)]
pub struct SpaHook {
    link: [*mut c_void; 2],
    cb: [*mut c_void; 2],
    removed: *mut c_void,
    private: *mut c_void,
}

impl SpaHook {
    pub fn new() -> Box<SpaHook> {
        Box::new(SpaHook { link: [std::ptr::null_mut(); 2], cb: [std::ptr::null_mut(); 2], removed: std::ptr::null_mut(), private: std::ptr::null_mut() })
    }
}

#[repr(C)]
pub struct SpaDictItem {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct SpaDict {
    pub flags: u32,
    pub n_items: u32,
    pub items: *const SpaDictItem,
}

#[repr(C)]
pub struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
pub struct SpaData {
    pub kind: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut SpaChunk,
}

#[repr(C)]
pub struct SpaBuffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut SpaData,
}

#[repr(C)]
pub struct PwBuffer {
    pub buffer: *mut SpaBuffer,
    pub user_data: *mut c_void,
    pub size: u64,
    pub requested: u64,
}

/// pw_stream_events (version 2), every slot so the offsets line up.
#[repr(C)]
pub struct StreamEvents {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    pub control_info: *const c_void,
    pub io_changed: *const c_void,
    pub param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const u8)>,
    pub add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut PwBuffer)>,
    pub process: Option<unsafe extern "C" fn(*mut c_void)>,
    pub drained: *const c_void,
    pub command: *const c_void,
    pub trigger_done: *const c_void,
}

// SAFETY: a table of function pointers and null pointers, never written after it is built
unsafe impl Sync for StreamEvents {}

#[repr(C)]
pub struct RegistryEvents {
    pub version: u32,
    pub global: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_char, u32, *const SpaDict)>,
    pub global_remove: Option<unsafe extern "C" fn(*mut c_void, u32)>,
}

#[repr(C)]
pub struct NodeEvents {
    pub version: u32,
    pub info: *const c_void,
    pub param: Option<unsafe extern "C" fn(*mut c_void, c_int, u32, u32, u32, *const u8)>,
}

#[repr(C)]
pub struct CoreEvents {
    pub version: u32,
    pub info: *const c_void,
    pub done: Option<unsafe extern "C" fn(*mut c_void, u32, c_int)>,
    pub ping: *const c_void,
    pub error: Option<unsafe extern "C" fn(*mut c_void, u32, c_int, c_int, *const c_char)>,
    pub remove_id: *const c_void,
    pub bound_id: *const c_void,
    pub add_mem: *const c_void,
    pub remove_mem: *const c_void,
    pub bound_props: *const c_void,
}

// SAFETY: same as StreamEvents
unsafe impl Sync for RegistryEvents {}
// SAFETY: same as StreamEvents
unsafe impl Sync for NodeEvents {}
// SAFETY: same as StreamEvents
unsafe impl Sync for CoreEvents {}

/// A spa_interface: the object every proxy starts with, whose callbacks point at a table of methods.
#[repr(C)]
struct SpaInterface {
    kind: *const c_char,
    version: u32,
    funcs: *const c_void,
    data: *mut c_void,
}

#[repr(C)]
struct CoreMethods {
    version: u32,
    add_listener: Option<unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const CoreEvents, *mut c_void) -> c_int>,
    hello: *const c_void,
    sync: Option<unsafe extern "C" fn(*mut c_void, u32, c_int) -> c_int>,
    pong: *const c_void,
    error: *const c_void,
    get_registry: Option<unsafe extern "C" fn(*mut c_void, u32, usize) -> *mut c_void>,
}

#[repr(C)]
struct RegistryMethods {
    version: u32,
    add_listener: Option<unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const RegistryEvents, *mut c_void) -> c_int>,
    bind: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_char, u32, usize) -> *mut c_void>,
}

#[repr(C)]
struct NodeMethods {
    version: u32,
    add_listener: Option<unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const NodeEvents, *mut c_void) -> c_int>,
    subscribe_params: Option<unsafe extern "C" fn(*mut c_void, *mut u32, u32) -> c_int>,
}

/// The methods table and its object of a proxy, for calling one of them.
/// Needs: `proxy` must be a live libpipewire proxy whose interface has the method table `T`.
unsafe fn methods<'a, T>(proxy: *mut c_void) -> Option<(&'a T, *mut c_void)> {
    let iface = proxy as *const SpaInterface;
    // SAFETY: a libpipewire proxy starts with its spa_interface, as the header's inline calls rely on
    let (funcs, data) = unsafe { ((*iface).funcs as *const T, (*iface).data) };
    // SAFETY: funcs is the method table of the interface the caller named, or null
    unsafe { funcs.as_ref() }.map(|table| (table, data))
}

/// pw_core_get_registry.
/// Needs: `core` must be a live pw_core.
pub unsafe fn core_get_registry(core: *mut c_void) -> *mut c_void {
    // SAFETY: the caller's core is a proxy with core methods
    match unsafe { methods::<CoreMethods>(core) } {
        Some((table, data)) => match table.get_registry {
            // SAFETY: the method's signature is pw_core_methods.get_registry
            Some(call) => unsafe { call(data, PW_VERSION_REGISTRY, 0) },
            None => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}

/// pw_core_add_listener.
/// Needs: `core` must be live, and `hook` and `data` must outlive the listener.
pub unsafe fn core_add_listener(core: *mut c_void, hook: *mut SpaHook, events: &'static CoreEvents, data: *mut c_void) {
    // SAFETY: as core_get_registry
    if let Some((table, object)) = unsafe { methods::<CoreMethods>(core) } {
        if let Some(call) = table.add_listener {
            // SAFETY: the method's signature is pw_core_methods.add_listener
            unsafe { call(object, hook, events, data) };
        }
    }
}

/// pw_core_sync: asks for a `done` event with `seq`, which comes after everything sent so far.
/// Needs: `core` must be a live pw_core.
pub unsafe fn core_sync(core: *mut c_void, seq: c_int) {
    // SAFETY: as core_get_registry
    if let Some((table, object)) = unsafe { methods::<CoreMethods>(core) } {
        if let Some(call) = table.sync {
            // SAFETY: the method's signature is pw_core_methods.sync
            unsafe { call(object, 0, seq) };
        }
    }
}

/// What spa_loop_methods.invoke runs on the loop thread.
type InvokeFunc = unsafe extern "C" fn(*mut c_void, bool, u32, *const c_void, usize, *mut c_void) -> c_int;

unsafe extern "C" fn nothing(_loop: *mut c_void, _async: bool, _seq: u32, _data: *const c_void, _size: usize, _user: *mut c_void) -> c_int {
    0
}

/// The head of spa_interface (type, version) and its callbacks (funcs, data), which is where a spa_loop starts.
#[repr(C)]
struct LoopInterface {
    kind: *const c_char,
    version: u32,
    funcs: *const LoopMethods,
    data: *mut c_void,
}

/// spa_loop_methods up to invoke.
#[repr(C)]
struct LoopMethods {
    version: u32,
    add_source: *const c_void,
    update_source: *const c_void,
    remove_source: *const c_void,
    invoke: Option<unsafe extern "C" fn(*mut c_void, InvokeFunc, u32, *const c_void, usize, bool, *mut c_void) -> c_int>,
}

/// pw_loop_invoke with a do-nothing function and block set: returns once the loop thread has run everything queued to it before, such as a pw_stream_trigger_process.
/// Needs: `pw_loop` must be a live pw_loop, and the thread loop must not be locked by the caller (the loop thread would wait for it and never run the call).
pub unsafe fn loop_drain(pw_loop: *mut c_void) {
    // SAFETY: a pw_loop starts with `system`, then `loop`, a pointer to the spa_loop whose interface head is LoopInterface
    let interface = unsafe { *(pw_loop.cast::<*const LoopInterface>().add(1)) };
    if interface.is_null() {
        return;
    }
    // SAFETY: the interface is live as long as the loop is
    let interface = unsafe { &*interface };
    if interface.funcs.is_null() {
        return;
    }
    // SAFETY: funcs is a spa_loop_methods table
    if let Some(invoke) = unsafe { (*interface.funcs).invoke } {
        // SAFETY: the method's signature is spa_loop_methods.invoke
        unsafe { invoke(interface.data, nothing, 0, std::ptr::null(), 0, true, std::ptr::null_mut()) };
    }
}

/// pw_registry_add_listener.
/// Needs: `registry` must be live, and `hook` and `data` must outlive the listener.
pub unsafe fn registry_add_listener(registry: *mut c_void, hook: *mut SpaHook, events: &'static RegistryEvents, data: *mut c_void) {
    // SAFETY: the caller's registry is a proxy with registry methods
    if let Some((table, object)) = unsafe { methods::<RegistryMethods>(registry) } {
        if let Some(call) = table.add_listener {
            // SAFETY: the method's signature is pw_registry_methods.add_listener
            unsafe { call(object, hook, events, data) };
        }
    }
}

/// pw_registry_bind for a node.
/// Needs: `registry` must be live.
pub unsafe fn registry_bind_node(registry: *mut c_void, id: u32) -> *mut c_void {
    // SAFETY: as registry_add_listener
    match unsafe { methods::<RegistryMethods>(registry) } {
        Some((table, object)) => match table.bind {
            Some(call) => {
                let kind = c"PipeWire:Interface:Node";
                // SAFETY: the method's signature is pw_registry_methods.bind
                unsafe { call(object, id, kind.as_ptr(), PW_VERSION_NODE, 0) }
            }
            None => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}

/// pw_node_add_listener and pw_node_subscribe_params for the given param ids.
/// Needs: `node` must be a live bound node proxy, and `hook` and `data` must outlive the listener.
pub unsafe fn node_listen(node: *mut c_void, hook: *mut SpaHook, events: &'static NodeEvents, data: *mut c_void, mut param_ids: Vec<u32>) {
    // SAFETY: the caller's node is a proxy with node methods
    if let Some((table, object)) = unsafe { methods::<NodeMethods>(node) } {
        if let Some(call) = table.add_listener {
            // SAFETY: the method's signature is pw_node_methods.add_listener
            unsafe { call(object, hook, events, data) };
        }
        if let Some(call) = table.subscribe_params {
            // SAFETY: the method's signature is pw_node_methods.subscribe_params
            unsafe { call(object, param_ids.as_mut_ptr(), param_ids.len() as u32) };
        }
    }
}

macro_rules! lib_functions {
    ($($field:ident: $symbol:literal = $ty:ty;)*) => {
        /// libpipewire's exported functions.
        pub struct Lib {
            _library: libloading::Library,
            $(pub $field: $ty,)*
        }

        impl Lib {
            fn open() -> Result<Lib, String> {
                // SAFETY: libpipewire's initialisers are harmless to run
                let library = unsafe { libloading::Library::new("libpipewire-0.3.so.0") }.map_err(|e| format!("libpipewire isn't available: {e}"))?;
                $(
                    // SAFETY: each symbol is declared with the signature of its prototype in the PipeWire headers
                    let symbol = unsafe { library.get::<$ty>(concat!($symbol, "\0").as_bytes()) }.map_err(|e| format!("libpipewire has no {}: {e}", $symbol))?;
                    let $field: $ty = *symbol;
                )*
                Ok(Lib { _library: library, $($field,)* })
            }
        }
    };
}

lib_functions! {
    init: "pw_init" = unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char);
    thread_loop_new: "pw_thread_loop_new" = unsafe extern "C" fn(*const c_char, *const c_void) -> *mut c_void;
    thread_loop_start: "pw_thread_loop_start" = unsafe extern "C" fn(*mut c_void) -> c_int;
    thread_loop_stop: "pw_thread_loop_stop" = unsafe extern "C" fn(*mut c_void);
    thread_loop_destroy: "pw_thread_loop_destroy" = unsafe extern "C" fn(*mut c_void);
    thread_loop_lock: "pw_thread_loop_lock" = unsafe extern "C" fn(*mut c_void);
    thread_loop_unlock: "pw_thread_loop_unlock" = unsafe extern "C" fn(*mut c_void);
    thread_loop_get_loop: "pw_thread_loop_get_loop" = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
    context_new: "pw_context_new" = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void;
    context_connect: "pw_context_connect" = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void;
    context_connect_fd: "pw_context_connect_fd" = unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, usize) -> *mut c_void;
    context_destroy: "pw_context_destroy" = unsafe extern "C" fn(*mut c_void);
    core_disconnect: "pw_core_disconnect" = unsafe extern "C" fn(*mut c_void) -> c_int;
    proxy_destroy: "pw_proxy_destroy" = unsafe extern "C" fn(*mut c_void);
    properties_new: "pw_properties_new" = unsafe extern "C" fn(*const c_char, ...) -> *mut c_void;
    properties_set: "pw_properties_set" = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
    stream_new: "pw_stream_new" = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void;
    stream_add_listener: "pw_stream_add_listener" = unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const StreamEvents, *mut c_void);
    stream_connect: "pw_stream_connect" = unsafe extern "C" fn(*mut c_void, c_int, u32, u32, *mut *const u8, u32) -> c_int;
    stream_update_params: "pw_stream_update_params" = unsafe extern "C" fn(*mut c_void, *mut *const u8, u32) -> c_int;
    stream_dequeue_buffer: "pw_stream_dequeue_buffer" = unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer;
    stream_queue_buffer: "pw_stream_queue_buffer" = unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int;
    stream_trigger_process: "pw_stream_trigger_process" = unsafe extern "C" fn(*mut c_void) -> c_int;
    stream_destroy: "pw_stream_destroy" = unsafe extern "C" fn(*mut c_void);
}

/// The loaded library, or why there isn't one. It stays loaded for the life of the program.
pub fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = Lib::open()?;
        // SAFETY: pw_init takes null arguments, and runs once
        unsafe { (lib.init)(std::ptr::null_mut(), std::ptr::null_mut()) };
        Ok(lib)
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// A pw_properties made from pairs, for the call that takes it over.
/// Needs: Only valid for a call that takes ownership of the properties (pw_context_new, pw_stream_new, ...).
pub unsafe fn properties(lib: &Lib, pairs: &[(&str, &str)]) -> *mut c_void {
    // SAFETY: a list that ends with its null terminator is pw_properties_new's calling convention
    let props = unsafe { (lib.properties_new)(std::ptr::null::<c_char>()) };
    if props.is_null() {
        return props;
    }
    for (key, value) in pairs {
        let (Ok(key), Ok(value)) = (std::ffi::CString::new(*key), std::ffi::CString::new(*value)) else { continue };
        // SAFETY: props is the live pw_properties made above, and the strings are copied
        unsafe { (lib.properties_set)(props, key.as_ptr(), value.as_ptr()) };
    }
    props
}

/// The pairs of a spa_dict.
/// Needs: `dict` must be null or point to a valid spa_dict for the length of the call.
pub unsafe fn dict_pairs(dict: *const SpaDict) -> Vec<(String, String)> {
    // SAFETY: the caller's dict is valid or null
    let Some(dict) = (unsafe { dict.as_ref() }) else { return Vec::new() };
    if dict.items.is_null() {
        return Vec::new();
    }
    // SAFETY: a spa_dict's items are n_items entries
    let items = unsafe { std::slice::from_raw_parts(dict.items, dict.n_items as usize) };
    items
        .iter()
        .filter(|item| !item.key.is_null() && !item.value.is_null())
        .map(|item| {
            // SAFETY: keys and values of a spa_dict are NUL-terminated strings
            unsafe { (std::ffi::CStr::from_ptr(item.key).to_string_lossy().into_owned(), std::ffi::CStr::from_ptr(item.value).to_string_lossy().into_owned()) }
        })
        .collect()
}
