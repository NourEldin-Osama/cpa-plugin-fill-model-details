//! Calls from the plugin back into CPA.

use std::ffi::{CString, c_void};
use std::ptr;

use base64::prelude::*;
use serde_json::{Value, json};

use crate::abi::{self, CliproxyBuffer};
use crate::error::PluginError;

pub struct HttpResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
}

/// Sends one host callback and returns the `result` part of CPA's reply.
pub fn call(method: &str, payload: &Value) -> Result<Value, PluginError> {
    let host = abi::host_api()
        .ok_or_else(|| PluginError::new("host_unavailable", "CPA gave no host callbacks"))?;
    let call_function = host
        .call
        .ok_or_else(|| PluginError::new("host_unavailable", "CPA gave no host call function"))?;
    let method_name = CString::new(method)
        .map_err(|_| PluginError::new("invalid_method", "method name contains a NUL byte"))?;
    let payload_bytes = serde_json::to_vec(payload)?;

    let mut response = CliproxyBuffer {
        ptr: ptr::null_mut(),
        len: 0,
    };
    // SAFETY: the function pointer and context come from CPA, and every
    // pointer passed here stays valid for the duration of the call.
    let return_code = unsafe {
        call_function(
            host.host_ctx,
            method_name.as_ptr(),
            payload_bytes.as_ptr(),
            payload_bytes.len(),
            &mut response,
        )
    };

    let response_bytes = if response.ptr.is_null() || response.len == 0 {
        Vec::new()
    } else {
        // SAFETY: CPA filled `response` with `len` readable bytes.
        unsafe { std::slice::from_raw_parts(response.ptr, response.len) }.to_vec()
    };
    if !response.ptr.is_null()
        && let Some(free_function) = host.free_buffer
    {
        // SAFETY: the buffer belongs to CPA and is freed exactly once.
        unsafe { free_function(response.ptr.cast::<c_void>(), response.len) };
    }

    if response_bytes.is_empty() {
        return Err(PluginError::new(
            "host_call_failed",
            format!("{method} returned code {return_code} and no data"),
        ));
    }
    let envelope: Value = serde_json::from_slice(&response_bytes)?;
    if envelope.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(envelope.get("result").cloned().unwrap_or(Value::Null));
    }
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    Err(PluginError::new(
        "host_call_failed",
        format!("{method}: {message}"),
    ))
}

/// GET through CPA's own HTTP client, so CPA's proxy settings apply.
pub fn http_get(url: &str, host_callback_id: Option<&str>) -> Result<HttpResponse, PluginError> {
    let mut payload = json!({
        "method": "GET",
        "url": url,
        "headers": { "Accept": ["application/json"] }
    });
    if let Some(callback_id) = host_callback_id {
        payload["host_callback_id"] = json!(callback_id);
    }
    let result = call("host.http.do", &payload)?;

    let status_code = field(&result, "StatusCode", "status_code")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0);
    let encoded_body = field(&result, "Body", "body")
        .and_then(Value::as_str)
        .unwrap_or("");
    let body = BASE64_STANDARD
        .decode(encoded_body)
        .map_err(|error| PluginError::new("invalid_body", error.to_string()))?;
    Ok(HttpResponse { status_code, body })
}

/// CPA encodes some structs with Go field names and others with snake_case.
fn field<'a>(value: &'a Value, go_name: &str, snake_name: &str) -> Option<&'a Value> {
    value.get(go_name).or_else(|| value.get(snake_name))
}
