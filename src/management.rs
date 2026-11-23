//! Management page and routes.
//!
//! The page is a GET resource that CPA serves without login, so it holds no
//! data. The routes live under `/v0/management/` and need the management key,
//! which CPA checks before the request reaches the plugin. The plugin passes
//! the caller's key on to CPA's own settings API and never stores it.
//!
//! Preview is a GET without a body: when CPA refuses a key on a request with
//! a body, it closes the connection, and the page would see a network error
//! instead of the 401 or 403 answer.

use base64::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cache::{self, CatalogStatus, Refresh};
use crate::error::PluginError;
use crate::fill::{self, ModelPreview, RequestedChange};
use crate::loopback;

const PLUGIN_SEGMENT: &str = "/fill-model-details/";
const PAGE_HTML: &str = include_str!("page.html");
const API_KEYS_PATH: &str = "/v8/management/config/api-keys";

pub fn registration() -> Value {
    json!({
        "routes": [
            {
                "Method": "GET",
                "Path": "/fill-model-details/preview",
                "Description": "List the empty model fields that models.dev can fill"
            },
            {
                "Method": "POST",
                "Path": "/fill-model-details/apply",
                "Description": "Save the picked model fields"
            }
        ],
        "resources": [
            {
                "Path": "/page",
                "Menu": "Fill Model Details",
                "Description": "Fill missing model details from models.dev"
            }
        ]
    })
}

pub fn handle(request: &Value) -> Result<Value, PluginError> {
    let method = text_field(request, "Method").to_ascii_uppercase();
    let path = text_field(request, "Path");
    let route = route_name(&path);
    let reply = match (method.as_str(), route.as_str()) {
        ("GET", "page") => reply(200, "text/html; charset=utf-8", PAGE_HTML.as_bytes()),
        ("GET", "preview") => route_reply(preview(request)),
        ("POST", "apply") => route_reply(apply(request)),
        _ => json_reply(404, &json!({ "error": "not_found" })),
    };
    Ok(reply)
}

#[derive(Serialize)]
struct PreviewReply {
    /// The config version the preview was made from; Save sends it back.
    etag: String,
    catalog: CatalogStatus,
    models: Vec<ModelPreview>,
}

#[derive(Deserialize)]
struct ApplyRequest {
    #[serde(default)]
    etag: String,
    #[serde(default)]
    changes: Vec<RequestedChange>,
}

#[derive(Serialize)]
struct ApplyReply {
    is_written: bool,
    applied_count: usize,
}

fn preview(request: &Value) -> Result<Value, PluginError> {
    let requested_port = query_value(request, "cpa_port").and_then(|port| port.parse().ok());
    let port = cpa_port(request, requested_port)?;
    let auth_headers = forwarded_auth_headers(request)?;
    let refresh = match query_value(request, "refresh") {
        Some("1" | "true") => Refresh::Always,
        _ => Refresh::WhenOld,
    };
    let host_callback_id = request.get("host_callback_id").and_then(Value::as_str);
    let loaded = cache::load(refresh, host_callback_id)?;
    let config = read_api_keys(port, &auth_headers)?;
    let preview_reply = PreviewReply {
        etag: config.etag,
        catalog: loaded.status,
        models: fill::plan(&config.api_keys, &loaded.catalog),
    };
    Ok(serde_json::to_value(preview_reply)?)
}

/// Saves the picked changes with one write, only if the config and the
/// models.dev data are still the ones the preview showed.
fn apply(request: &Value) -> Result<Value, PluginError> {
    let body = request_body_json(request)?;
    let apply_request: ApplyRequest = serde_json::from_value(body.clone())
        .map_err(|error| PluginError::new("invalid_request", error.to_string()))?;
    let nothing_written = ApplyReply {
        is_written: false,
        applied_count: 0,
    };
    if apply_request.changes.is_empty() {
        return Ok(serde_json::to_value(nothing_written)?);
    }
    if apply_request.etag.trim().is_empty() {
        return Err(PluginError::new(
            "invalid_request",
            "the request has no config version (etag)",
        ));
    }
    let requested_port = body
        .get("cpa_port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok());
    let port = cpa_port(request, requested_port)?;
    let auth_headers = forwarded_auth_headers(request)?;

    let loaded = cache::load(Refresh::Never, None).map_err(|_| {
        PluginError::new(
            "preview_outdated",
            "the models.dev data from the preview is gone",
        )
    })?;
    let mut config = read_api_keys(port, &auth_headers)?;
    if config.etag != apply_request.etag {
        return Err(config_changed_error());
    }
    let previews = fill::plan(&config.api_keys, &loaded.catalog);
    let picked_changes = fill::select(&previews, &apply_request.changes)?;
    let applied_count = fill::apply(&mut config.api_keys, &picked_changes);
    if applied_count == 0 {
        return Ok(serde_json::to_value(nothing_written)?);
    }

    let write_body = serde_json::to_vec(&config.api_keys)?;
    let mut write_headers = auth_headers;
    write_headers.push(("If-Match".to_string(), config.etag));
    let response = loopback::send(
        port,
        "PUT",
        API_KEYS_PATH,
        &write_headers,
        Some(&write_body),
    )?;
    match response.status_code {
        200 | 204 => Ok(serde_json::to_value(ApplyReply {
            is_written: true,
            applied_count,
        })?),
        412 => Err(config_changed_error()),
        401 | 403 => Err(key_rejected_error()),
        status_code => Err(PluginError::new(
            "save_failed",
            format!(
                "CPA answered HTTP {status_code} ({})",
                error_code(&response.body)
            ),
        )),
    }
}

struct ApiKeysConfig {
    etag: String,
    api_keys: Value,
}

fn read_api_keys(
    port: u16,
    auth_headers: &[(String, String)],
) -> Result<ApiKeysConfig, PluginError> {
    let response = loopback::send(port, "GET", API_KEYS_PATH, auth_headers, None)?;
    match response.status_code {
        200 => {}
        401 | 403 => return Err(key_rejected_error()),
        status_code => {
            return Err(PluginError::new(
                "read_failed",
                format!(
                    "CPA answered HTTP {status_code} ({})",
                    error_code(&response.body)
                ),
            ));
        }
    }
    let etag = response.header("ETag").unwrap_or("").trim().to_string();
    if etag.is_empty() {
        return Err(PluginError::new(
            "read_failed",
            "CPA sent no config version (ETag), so a safe save is not possible",
        ));
    }
    let api_keys: Value = serde_json::from_slice(&response.body)?;
    if !api_keys.is_object() {
        return Err(PluginError::new(
            "read_failed",
            "CPA sent the provider list in an unexpected shape",
        ));
    }
    Ok(ApiKeysConfig { etag, api_keys })
}

fn config_changed_error() -> PluginError {
    PluginError::new("config_changed", "the config changed after the preview")
}

fn key_rejected_error() -> PluginError {
    PluginError::new("key_rejected", "CPA did not accept the management key")
}

fn route_reply(result: Result<Value, PluginError>) -> Value {
    match result {
        Ok(body) => json_reply(200, &body),
        Err(error) => error_reply(error_status(&error.code), &error),
    }
}

fn error_status(code: &str) -> u16 {
    match code {
        "missing_key" | "key_rejected" => 401,
        "missing_port" | "invalid_json" | "invalid_body" | "invalid_request" => 400,
        "config_changed" | "preview_outdated" => 409,
        "catalog_unavailable" => 503,
        _ => 502,
    }
}

/// Path after the plugin segment, for both route and resource paths.
fn route_name(path: &str) -> String {
    let path_only = path.split('?').next().unwrap_or("");
    match path_only.find(PLUGIN_SEGMENT) {
        Some(index) => path_only[index + PLUGIN_SEGMENT.len()..].to_string(),
        None => path_only.trim_start_matches('/').to_string(),
    }
}

/// Port of this CPA instance. Go drops `Host` from the header map, so the
/// page also sends its own port; Origin and Referer are the last fallback.
fn cpa_port(request: &Value, requested_port: Option<u16>) -> Result<u16, PluginError> {
    let from_host = header_value(request, "Host").and_then(loopback::port_from_host);
    let from_request = requested_port.filter(|port| *port != 0);
    let from_origin = header_value(request, "Origin").and_then(loopback::port_from_url);
    let from_referer = header_value(request, "Referer").and_then(loopback::port_from_url);
    from_host
        .or(from_request)
        .or(from_origin)
        .or(from_referer)
        .ok_or_else(|| PluginError::new("missing_port", "could not tell which port CPA uses"))
}

fn query_value<'a>(request: &'a Value, name: &str) -> Option<&'a str> {
    match request.get("Query")?.get(name)? {
        Value::Array(items) => items.first().and_then(Value::as_str),
        Value::String(text) => Some(text.as_str()),
        _ => None,
    }
}

/// The caller's management key header, passed on unchanged. Never logged.
fn forwarded_auth_headers(request: &Value) -> Result<Vec<(String, String)>, PluginError> {
    let mut headers = Vec::new();
    for name in ["Authorization", "X-Management-Key"] {
        if let Some(value) = header_value(request, name) {
            headers.push((name.to_string(), value.to_string()));
        }
    }
    if headers.is_empty() {
        return Err(PluginError::new(
            "missing_key",
            "the request carried no management key",
        ));
    }
    Ok(headers)
}

fn header_value<'a>(request: &'a Value, name: &str) -> Option<&'a str> {
    let headers = request.get("Headers")?.as_object()?;
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .and_then(|(_, values)| match values {
            Value::Array(items) => items.first().and_then(Value::as_str),
            Value::String(text) => Some(text.as_str()),
            _ => None,
        })
}

fn request_body_json(request: &Value) -> Result<Value, PluginError> {
    let encoded_body = text_field(request, "Body");
    if encoded_body.is_empty() {
        return Ok(json!({}));
    }
    let body_bytes = BASE64_STANDARD
        .decode(encoded_body.as_bytes())
        .map_err(|error| PluginError::new("invalid_body", error.to_string()))?;
    if body_bytes.is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_slice(&body_bytes)?)
}

/// Only the `error` code of a CPA reply, so config text never leaves the plugin.
fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|reply| {
            reply
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "no error code".to_string())
}

fn text_field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn reply(status_code: u16, content_type: &str, body: &[u8]) -> Value {
    json!({
        "StatusCode": status_code,
        "Headers": {
            "Content-Type": [content_type],
            "Cache-Control": ["no-store"]
        },
        "Body": BASE64_STANDARD.encode(body),
    })
}

fn json_reply(status_code: u16, body: &Value) -> Value {
    let body_bytes = serde_json::to_vec(body).unwrap_or_default();
    reply(status_code, "application/json; charset=utf-8", &body_bytes)
}

fn error_reply(status_code: u16, error: &PluginError) -> Value {
    json_reply(
        status_code,
        &json!({ "error": error.code, "message": error.message }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with(headers: Value, body: &Value) -> Value {
        json!({
            "Method": "POST",
            "Path": "/v0/management/fill-model-details/apply",
            "Headers": headers,
            "Body": BASE64_STANDARD.encode(serde_json::to_vec(body).expect("JSON")),
        })
    }

    #[test]
    fn finds_routes_and_pages() {
        assert_eq!(
            route_name("/v0/management/fill-model-details/preview"),
            "preview"
        );
        assert_eq!(
            route_name("/v0/resource/plugins/fill-model-details/page"),
            "page"
        );
        let unknown =
            handle(&json!({ "Method": "GET", "Path": "/v0/management/fill-model-details/x" }))
                .expect("a reply");
        assert_eq!(unknown["StatusCode"], json!(404));
        let page = handle(
            &json!({ "Method": "GET", "Path": "/v0/resource/plugins/fill-model-details/page" }),
        )
        .expect("a reply");
        assert_eq!(page["StatusCode"], json!(200));
    }

    #[test]
    fn reads_port_and_key_from_the_request() {
        let request = json!({
            "Method": "GET",
            "Path": "/v0/management/fill-model-details/preview",
            "Headers": {
                "Authorization": ["Bearer test-key"],
                "Referer": ["http://127.0.0.1:9999/v0/resource/plugins/fill-model-details/page"]
            },
            "Query": { "cpa_port": ["8318"], "refresh": ["1"] },
        });
        assert_eq!(query_value(&request, "cpa_port"), Some("8318"));
        assert_eq!(query_value(&request, "missing"), None);
        assert_eq!(cpa_port(&request, Some(8318)).expect("port"), 8318);
        assert_eq!(cpa_port(&request, Some(0)).expect("port"), 9999);
        let headers = forwarded_auth_headers(&request).expect("key header");
        assert_eq!(
            headers,
            [("Authorization".to_string(), "Bearer test-key".to_string())]
        );

        let without_key = json!({ "Headers": { "Origin": ["http://127.0.0.1:9999"] } });
        assert_eq!(cpa_port(&without_key, None).expect("port"), 9999);
        let error = forwarded_auth_headers(&without_key).expect_err("no key");
        assert_eq!(error_status(&error.code), 401);
        assert!(cpa_port(&json!({}), None).is_err());
    }

    #[test]
    fn apply_with_nothing_picked_writes_nothing() {
        // No port and no key are needed, because nothing is read or written.
        let request = request_with(json!({}), &json!({ "etag": "\"x\"", "changes": [] }));
        let result = apply(&request).expect("nothing to do is not an error");
        assert_eq!(result, json!({ "is_written": false, "applied_count": 0 }));
    }
}
