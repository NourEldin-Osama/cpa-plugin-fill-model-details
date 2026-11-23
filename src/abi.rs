//! C entry points that CPA calls.
//!
//! Every call is wrapped in `catch_unwind`, so a Rust panic is turned into an
//! error reply instead of crossing into CPA and stopping the whole proxy.
//! Buffers handed to CPA are boxed slices, so their length equals their
//! capacity when CPA gives them back to `plugin_free`.

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use serde_json::json;

use crate::error::PluginError;
use crate::plugin;

const ABI_VERSION: u32 = 1;

#[repr(C)]
pub struct CliproxyBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

pub type HostCallFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, usize, *mut CliproxyBuffer) -> i32;
pub type HostFreeFn = unsafe extern "C" fn(*mut c_void, usize);
type PluginCallFn =
    unsafe extern "C" fn(*const c_char, *const u8, usize, *mut CliproxyBuffer) -> i32;
type PluginFreeFn = unsafe extern "C" fn(*mut c_void, usize);
type PluginShutdownFn = unsafe extern "C" fn();

#[repr(C)]
pub struct CliproxyHostApi {
    pub abi_version: u32,
    pub host_ctx: *mut c_void,
    pub call: Option<HostCallFn>,
    pub free_buffer: Option<HostFreeFn>,
}

#[repr(C)]
pub struct CliproxyPluginApi {
    pub abi_version: u32,
    pub call: Option<PluginCallFn>,
    pub free_buffer: Option<PluginFreeFn>,
    pub shutdown: Option<PluginShutdownFn>,
}

/// Host function table given to `cliproxy_plugin_init`. CPA keeps it alive
/// while the plugin is loaded. An atomic pointer (not `static mut`) lets a
/// second init replace it safely.
static HOST_API: AtomicPtr<CliproxyHostApi> = AtomicPtr::new(ptr::null_mut());

pub fn host_api() -> Option<&'static CliproxyHostApi> {
    let host_pointer = HOST_API.load(Ordering::Acquire);
    if host_pointer.is_null() {
        return None;
    }
    // SAFETY: CPA guarantees the table outlives the loaded plugin.
    Some(unsafe { &*host_pointer })
}

/// # Safety
/// Called by CPA with a valid plugin table to fill in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cliproxy_plugin_init(
    host: *const CliproxyHostApi,
    plugin_api: *mut CliproxyPluginApi,
) -> i32 {
    if plugin_api.is_null() {
        return 1;
    }
    HOST_API.store(host.cast_mut(), Ordering::Release);
    // SAFETY: checked for null above; CPA owns the memory.
    unsafe {
        (*plugin_api).abi_version = ABI_VERSION;
        (*plugin_api).call = Some(plugin_call);
        (*plugin_api).free_buffer = Some(plugin_free);
        (*plugin_api).shutdown = Some(plugin_shutdown);
    }
    0
}

unsafe extern "C" fn plugin_call(
    method: *const c_char,
    request: *const u8,
    request_len: usize,
    response: *mut CliproxyBuffer,
) -> i32 {
    if response.is_null() {
        return 1;
    }
    // SAFETY: checked for null above.
    unsafe {
        (*response).ptr = ptr::null_mut();
        (*response).len = 0;
    }

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: CPA passes a NUL-terminated method name and a request
        // buffer that stays valid for this call.
        let method_name = unsafe { read_method(method) }?;
        let request_bytes = unsafe { read_request(request, request_len) };
        plugin::handle_method(&method_name, &request_bytes)
    }));

    let envelope = match outcome {
        Ok(Ok(result)) => json!({ "ok": true, "result": result }),
        Ok(Err(error)) => json!({
            "ok": false,
            "error": { "code": error.code, "message": error.message }
        }),
        Err(_) => json!({
            "ok": false,
            "error": { "code": "plugin_panic", "message": "the plugin hit an internal error" }
        }),
    };
    let encoded = serde_json::to_vec(&envelope).unwrap_or_else(|_| {
        br#"{"ok":false,"error":{"code":"encode_failed","message":"could not encode the reply"}}"#
            .to_vec()
    });
    // SAFETY: checked for null above.
    unsafe { write_buffer(response, encoded) };
    0
}

unsafe extern "C" fn plugin_free(pointer: *mut c_void, len: usize) {
    if pointer.is_null() {
        return;
    }
    let slice_pointer = ptr::slice_from_raw_parts_mut(pointer.cast::<u8>(), len);
    // SAFETY: the pointer came from `write_buffer`, which leaked a boxed slice
    // of exactly `len` bytes.
    drop(unsafe { Box::from_raw(slice_pointer) });
}

unsafe extern "C" fn plugin_shutdown() {}

unsafe fn read_method(method: *const c_char) -> Result<String, PluginError> {
    if method.is_null() {
        return Err(PluginError::new("invalid_method", "method is required"));
    }
    // SAFETY: the caller promises a NUL-terminated string.
    let method_text = unsafe { CStr::from_ptr(method) };
    method_text
        .to_str()
        .map(str::to_string)
        .map_err(|_| PluginError::new("invalid_method", "method is not UTF-8"))
}

unsafe fn read_request(request: *const u8, request_len: usize) -> Vec<u8> {
    if request.is_null() || request_len == 0 {
        return Vec::new();
    }
    // SAFETY: the caller promises `request_len` readable bytes.
    unsafe { std::slice::from_raw_parts(request, request_len) }.to_vec()
}

unsafe fn write_buffer(response: *mut CliproxyBuffer, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    let boxed_bytes = bytes.into_boxed_slice();
    let len = boxed_bytes.len();
    let pointer = Box::into_raw(boxed_bytes).cast::<u8>();
    // SAFETY: the caller checked `response` for null.
    unsafe {
        (*response).ptr = pointer;
        (*response).len = len;
    }
}
