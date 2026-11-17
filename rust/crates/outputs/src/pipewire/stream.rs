//! A PipeWire stream of either direction on a shared connection: made with its properties and the formats it offers,
//! and run through a handler that gets the callbacks on the loop thread. The video consumer is one handler, a producer
//! would be another.

use super::connection::Connection;
use super::ffi::{self, PwBuffer, SpaHook, StreamEvents, StreamState};
use super::pod::Pod;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Arc;

pub use ffi::{PW_DIRECTION_INPUT, PW_DIRECTION_OUTPUT, PW_STREAM_FLAG_ALLOC_BUFFERS, PW_STREAM_FLAG_AUTOCONNECT, PW_STREAM_FLAG_DRIVER, SPA_ID_INVALID};

/// What a handler can do to its stream from inside a callback.
pub struct StreamRef {
    lib: &'static ffi::Lib,
    raw: *mut c_void,
}

impl StreamRef {
    /// Replaces the stream's params (formats, then buffer requirements after a format settles).
    pub fn update_params(&self, params: &[Pod]) {
        let mut pointers: Vec<*const u8> = params.iter().map(Pod::as_ptr).collect();
        // SAFETY: raw is the live stream, and the pods outlive the call, which copies them
        unsafe { (self.lib.stream_update_params)(self.raw, pointers.as_mut_ptr(), pointers.len() as u32) };
    }

    /// The next buffer a peer has filled, or null.
    pub fn dequeue(&self) -> *mut PwBuffer {
        // SAFETY: raw is the live stream
        unsafe { (self.lib.stream_dequeue_buffer)(self.raw) }
    }

    /// Hands a buffer back.
    ///
    /// # Safety
    /// The buffer must have been dequeued from this stream and not queued since.
    pub unsafe fn queue(&self, buffer: *mut PwBuffer) {
        // SAFETY: raw is the live stream, and the buffer came from it
        unsafe { (self.lib.stream_queue_buffer)(self.raw, buffer) };
    }
}

/// The callbacks of a stream, all on the loop thread.
pub trait StreamHandler: Send + 'static {
    fn state_changed(&mut self, _stream: &StreamRef, _state: StreamState, _error: Option<String>) {}
    /// A param changed: None for the pod when it was cleared.
    fn param_changed(&mut self, _stream: &StreamRef, _id: u32, _pod: Option<super::pod::ObjectPod>) {}
    fn add_buffer(&mut self, _stream: &StreamRef, _buffer: *mut PwBuffer) {}
    fn remove_buffer(&mut self, _stream: &StreamRef, _buffer: *mut PwBuffer) {}
    fn process(&mut self, _stream: &StreamRef) {}
}

struct Inner {
    stream: StreamRef,
    handler: Box<dyn StreamHandler>,
}

fn guarded(body: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
}

static EVENTS: StreamEvents = StreamEvents {
    version: 2,
    destroy: None,
    state_changed: Some(on_state_changed),
    control_info: std::ptr::null(),
    io_changed: std::ptr::null(),
    param_changed: Some(on_param_changed),
    add_buffer: Some(on_add_buffer),
    remove_buffer: Some(on_remove_buffer),
    process: Some(on_process),
    drained: std::ptr::null(),
    command: std::ptr::null(),
    trigger_done: std::ptr::null(),
};

unsafe extern "C" fn on_state_changed(data: *mut c_void, _old: c_int, state: c_int, error: *const c_char) {
    guarded(|| {
        // SAFETY: data is the Inner the listener was added with, which lives until the stream is destroyed
        let inner = unsafe { &mut *(data as *mut Inner) };
        // SAFETY: error is null or a NUL-terminated string
        let error = (!error.is_null()).then(|| unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned());
        inner.handler.state_changed(&inner.stream, StreamState::from_raw(state), error);
    });
}

unsafe extern "C" fn on_param_changed(data: *mut c_void, id: u32, param: *const u8) {
    guarded(|| {
        // SAFETY: as on_state_changed
        let inner = unsafe { &mut *(data as *mut Inner) };
        // SAFETY: param is null or a whole pod for this call
        let pod = unsafe { super::pod::ObjectPod::from_ptr(param) };
        inner.handler.param_changed(&inner.stream, id, pod);
    });
}

unsafe extern "C" fn on_add_buffer(data: *mut c_void, buffer: *mut PwBuffer) {
    guarded(|| {
        // SAFETY: as on_state_changed
        let inner = unsafe { &mut *(data as *mut Inner) };
        inner.handler.add_buffer(&inner.stream, buffer);
    });
}

unsafe extern "C" fn on_remove_buffer(data: *mut c_void, buffer: *mut PwBuffer) {
    guarded(|| {
        // SAFETY: as on_state_changed
        let inner = unsafe { &mut *(data as *mut Inner) };
        inner.handler.remove_buffer(&inner.stream, buffer);
    });
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    guarded(|| {
        // SAFETY: as on_state_changed
        let inner = unsafe { &mut *(data as *mut Inner) };
        inner.handler.process(&inner.stream);
    });
}

/// How a stream is made.
#[derive(Clone, Copy)]
pub struct StreamConfig<'a> {
    pub name: &'a str,
    pub props: &'a [(&'a str, &'a str)],
    /// PW_DIRECTION_INPUT or PW_DIRECTION_OUTPUT.
    pub direction: i32,
    /// The node to connect to, or SPA_ID_INVALID to let the session manager choose.
    pub target: u32,
    pub flags: u32,
}

/// A stream on a connection, ended when dropped.
pub struct Stream {
    connection: Arc<Connection>,
    raw: *mut c_void,
    handle: StreamRef,
    _hook: Box<SpaHook>,
    _inner: Box<Inner>,
}

// SAFETY: the stream is only used with the loop locked, and its handler is Send
unsafe impl Send for Stream {}
// SAFETY: as above, the one method taking &self locks the loop
unsafe impl Sync for Stream {}

impl Stream {
    /// Makes a stream as `config` says, offering `params`, and runs `handler` for it.
    pub fn connect(connection: &Arc<Connection>, config: &StreamConfig, params: &[Pod], handler: Box<dyn StreamHandler>) -> Result<Stream, String> {
        let lib = connection.lib();
        let StreamConfig { name, props, direction, target, flags } = *config;
        let name = CString::new(name).map_err(|_| "a stream name has a NUL in it".to_string())?;
        connection.locked(|| {
            // SAFETY: the properties go straight to pw_stream_new, which takes them over
            let props = unsafe { ffi::properties(lib, props) };
            // SAFETY: the core is live while the connection is, and the loop is locked
            let raw = unsafe { (lib.stream_new)(connection.core(), name.as_ptr(), props) };
            if raw.is_null() {
                return Err("PipeWire wouldn't make a stream".to_string());
            }
            let mut inner = Box::new(Inner { stream: StreamRef { lib, raw }, handler });
            let mut hook = SpaHook::new();
            // SAFETY: the hook and the inner are boxed and kept in the Stream until the stream is destroyed
            unsafe { (lib.stream_add_listener)(raw, &mut *hook, &EVENTS, &mut *inner as *mut Inner as *mut c_void) };
            let mut pointers: Vec<*const u8> = params.iter().map(Pod::as_ptr).collect();
            // SAFETY: the stream is live and the pods outlive the call, which copies them
            let result = unsafe { (lib.stream_connect)(raw, direction, target, flags, pointers.as_mut_ptr(), pointers.len() as u32) };
            if result < 0 {
                // SAFETY: the stream is live and destroyed once
                unsafe { (lib.stream_destroy)(raw) };
                return Err(format!("PipeWire wouldn't connect the stream ({})", std::io::Error::from_raw_os_error(-result)));
            }
            Ok(Stream { connection: connection.clone(), raw, handle: StreamRef { lib, raw }, _hook: hook, _inner: inner })
        })
    }

    /// Runs `body` with the loop locked and the stream to call into, for a short moment: the loop thread's callbacks wait while it runs.
    pub fn locked<R>(&self, body: impl FnOnce(&StreamRef) -> R) -> R {
        self.connection.locked(|| body(&self.handle))
    }

    /// Asks a driving stream for a cycle (what a lazy or timed producer does when it has a frame).
    pub fn trigger_process(&self) {
        let lib = self.connection.lib();
        self.connection.locked(|| {
            // SAFETY: the stream is live and the loop is locked
            unsafe { (lib.stream_trigger_process)(self.raw) };
        });
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let lib = self.connection.lib();
        self.connection.locked(|| {
            // SAFETY: the stream is live and destroyed once, with the loop locked so no callback is running
            unsafe { (lib.stream_destroy)(self.raw) };
        });
    }
}
