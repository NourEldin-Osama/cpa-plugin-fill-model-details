//! Plugin methods that CPA calls by name.

use serde_json::{Value, json};

use crate::error::PluginError;
use crate::management;

/// Plugin JSON contract version. 6 is the first version where CPA sends JSON
/// replies from management routes without HTML escaping. CPA v8.0.80 accepts
/// versions up to 10.
const SCHEMA_VERSION: u32 = 6;

pub fn handle_method(method: &str, request: &[u8]) -> Result<Value, PluginError> {
    match method {
        "plugin.register" | "plugin.reconfigure" => Ok(registration()),
        "plugin.shutdown" => Ok(Value::Null),
        "management.register" => Ok(management::registration()),
        "management.handle" => {
            let management_request: Value = serde_json::from_slice(request)?;
            management::handle(&management_request)
        }
        _ => Err(PluginError::new(
            "unknown_method",
            format!("unknown method: {method}"),
        )),
    }
}

fn registration() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "metadata": {
            "Name": "Fill Model Details",
            "Version": env!("CARGO_PKG_VERSION"),
            "Author": "NourEldin Osama",
            "GitHubRepository": "https://github.com/NourEldin-Osama/cpa-plugin-fill-model-details",
            "ConfigFields": []
        },
        "capabilities": { "management_api": true }
    })
}
