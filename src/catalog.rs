//! models.dev catalog: parsing the data and finding the entry for a model.
//!
//! Matching uses the exact model id only (ignoring letter case), never similar
//! names. Values differ between hosts, so the order is:
//! 1. the models.dev provider whose API host is the entry's base URL host,
//! 2. the model maker: either the id starts with the maker's provider id
//!    (`moonshotai/kimi-k3.1-pro` is `kimi-k3.1-pro` under `moonshotai`), or
//!    other hosts name the maker in `canonical_model_id`
//!    (`gpt-5.5` on a reseller points to `openai/gpt-5.5`),
//! 3. any other provider with the same id, in provider id order.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::error::PluginError;

/// Thinking level names CPA understands, in CPA's order. `none` tells CPA
/// that thinking can be turned off.
const THINKING_LEVEL_ORDER: [&str; 7] =
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub name: Option<String>,
    pub context_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub input_modalities: Vec<String>,
    pub reasoning: bool,
    /// Effort levels from `reasoning_options`, in CPA's order. `None` when
    /// models.dev lists no effort values.
    pub thinking_levels: Option<Vec<String>>,
    /// Thinking can only be switched on or off, with no levels.
    pub is_thinking_toggle_only: bool,
    pub tool_call: Option<bool>,
    /// The maker's id for this model, as `maker/model`.
    pub canonical_model_id: Option<String>,
    pub status: Option<String>,
    /// Missing for chat models; set for embedding, image, speech and others.
    pub model_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
    pub api_host: Option<String>,
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    SameHost,
    ModelMaker,
    OtherHost,
}

#[derive(Debug, Clone, Copy)]
pub struct CatalogMatch<'a> {
    pub provider: &'a CatalogProvider,
    pub model: &'a CatalogModel,
    pub kind: MatchKind,
}

type ProviderModel<'a> = (&'a CatalogProvider, &'a CatalogModel);

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    /// Sorted by provider id, so "other host" matches are stable.
    providers: Vec<CatalogProvider>,
}

impl Catalog {
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginError> {
        let root: Value = serde_json::from_slice(bytes)?;
        let provider_map = root.as_object().ok_or_else(|| {
            PluginError::new("invalid_catalog", "models.dev data is not an object")
        })?;
        let mut providers: Vec<CatalogProvider> = provider_map
            .iter()
            .filter_map(|(provider_key, provider_value)| {
                parse_provider(provider_key, provider_value)
            })
            .collect();
        if providers.is_empty() {
            return Err(PluginError::new(
                "invalid_catalog",
                "models.dev data has no providers",
            ));
        }
        providers.sort_by(|first, second| first.id.cmp(&second.id));
        Ok(Self { providers })
    }

    pub fn model_count(&self) -> usize {
        self.providers
            .iter()
            .map(|provider| provider.models.len())
            .sum()
    }

    pub fn find(&self, base_url: &str, model_id: &str) -> Option<CatalogMatch<'_>> {
        let entry_host = host_of_url(base_url);
        let exact_matches: Vec<ProviderModel<'_>> = self
            .providers
            .iter()
            .filter_map(|provider| {
                provider_model(provider, model_id).map(|model| (provider, model))
            })
            .collect();

        let same_host_match =
            exact_matches
                .iter()
                .find(|(provider, _)| match (&entry_host, &provider.api_host) {
                    (Some(entry), Some(api)) => hosts_match(entry, api),
                    _ => false,
                });
        if let Some(&(provider, model)) = same_host_match {
            return Some(CatalogMatch {
                provider,
                model,
                kind: MatchKind::SameHost,
            });
        }

        let maker_match = self.qualified_model(model_id).or_else(|| {
            most_common_canonical_id(&exact_matches)
                .and_then(|canonical_id| self.qualified_model(&canonical_id))
        });
        if let Some((provider, model)) = maker_match {
            return Some(CatalogMatch {
                provider,
                model,
                kind: MatchKind::ModelMaker,
            });
        }

        exact_matches
            .first()
            .map(|&(provider, model)| CatalogMatch {
                provider,
                model,
                kind: MatchKind::OtherHost,
            })
    }

    /// Finds `maker/model` under the maker's provider, which lists it either
    /// as `model` or as the full `maker/model`.
    fn qualified_model(&self, qualified_id: &str) -> Option<ProviderModel<'_>> {
        let (maker_id, maker_model_id) = qualified_id.split_once('/')?;
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id.eq_ignore_ascii_case(maker_id))?;
        let model = provider_model(provider, maker_model_id)
            .or_else(|| provider_model(provider, qualified_id))?;
        Some((provider, model))
    }
}

/// The `canonical_model_id` most hosts agree on; ties go to the first in
/// alphabetical order.
fn most_common_canonical_id(matches: &[ProviderModel<'_>]) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, model) in matches {
        if let Some(canonical_id) = &model.canonical_model_id {
            *counts.entry(canonical_id.to_ascii_lowercase()).or_default() += 1;
        }
    }
    let highest_count = counts.values().copied().max()?;
    counts
        .into_iter()
        .find(|(_, count)| *count == highest_count)
        .map(|(canonical_id, _)| canonical_id)
}

fn provider_model<'a>(provider: &'a CatalogProvider, model_id: &str) -> Option<&'a CatalogModel> {
    provider
        .models
        .iter()
        .find(|model| model.id.eq_ignore_ascii_case(model_id))
}

fn parse_provider(provider_key: &str, provider_value: &Value) -> Option<CatalogProvider> {
    let provider_object = provider_value.as_object()?;
    let id = text(provider_value, "id").unwrap_or_else(|| provider_key.to_string());
    let name = text(provider_value, "name").unwrap_or_else(|| id.clone());
    let api_host = text(provider_value, "api").and_then(|api| host_of_url(&api));
    let models = provider_object
        .get("models")
        .and_then(Value::as_object)
        .map(|model_map| {
            model_map
                .iter()
                .filter_map(|(model_key, model_value)| parse_model(model_key, model_value))
                .collect()
        })
        .unwrap_or_default();
    Some(CatalogProvider {
        id,
        name,
        api_host,
        models,
    })
}

fn parse_model(model_key: &str, model_value: &Value) -> Option<CatalogModel> {
    if !model_value.is_object() {
        return None;
    }
    let input_modalities = model_value
        .pointer("/modalities/input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| item.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let reasoning_options = model_value
        .get("reasoning_options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let thinking_levels = effort_levels(reasoning_options);
    let has_toggle = reasoning_options
        .iter()
        .any(|option| option.get("type").and_then(Value::as_str) == Some("toggle"));

    Some(CatalogModel {
        id: text(model_value, "id").unwrap_or_else(|| model_key.to_string()),
        name: text(model_value, "name"),
        context_tokens: positive_number(model_value.pointer("/limit/context")),
        output_tokens: positive_number(model_value.pointer("/limit/output")),
        input_modalities,
        reasoning: model_value
            .get("reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        is_thinking_toggle_only: has_toggle && thinking_levels.is_none(),
        thinking_levels,
        tool_call: model_value.get("tool_call").and_then(Value::as_bool),
        canonical_model_id: text(model_value, "canonical_model_id"),
        status: text(model_value, "status"),
        model_type: text(model_value, "type"),
    })
}

/// Effort values that CPA understands, in CPA's order. `default` is a
/// models.dev marker, not a level, so it is dropped with other unknown words.
fn effort_levels(reasoning_options: &[Value]) -> Option<Vec<String>> {
    let effort_values: Vec<String> = reasoning_options
        .iter()
        .filter(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
        .filter_map(|option| option.get("values").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .collect();
    let levels: Vec<String> = THINKING_LEVEL_ORDER
        .iter()
        .filter(|level| effort_values.iter().any(|value| value == *level))
        .map(|level| level.to_string())
        .collect();
    if levels.is_empty() {
        None
    } else {
        Some(levels)
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn positive_number(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|number| *number > 0)
}

/// Lowercase host of a URL, without user info or port.
pub fn host_of_url(url: &str) -> Option<String> {
    let trimmed_url = url.trim();
    let without_scheme = trimmed_url
        .split_once("://")
        .map_or(trimmed_url, |(_, rest)| rest);
    let authority = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let without_user = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if without_user.starts_with('[') {
        without_user
            .split(']')
            .next()
            .map(|part| format!("{part}]"))?
    } else {
        without_user.split(':').next().unwrap_or("").to_string()
    };
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// True when the entry host is the API host or one of its subdomains.
fn hosts_match(entry_host: &str, api_host: &str) -> bool {
    entry_host == api_host || entry_host.ends_with(&format!(".{api_host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_catalog() -> Catalog {
        let bytes = include_bytes!("../tests/fixtures/models-dev-sample.json");
        Catalog::parse(bytes).expect("fixture should parse")
    }

    fn find_in_sample(base_url: &str, model_id: &str) -> (MatchKind, String, String) {
        let catalog = sample_catalog();
        let found = catalog
            .find(base_url, model_id)
            .expect("the model should be found");
        (
            found.kind,
            found.provider.id.clone(),
            found.model.id.clone(),
        )
    }

    #[test]
    fn reads_hosts() {
        assert_eq!(
            host_of_url("https://integrate.api.nvidia.com/v1").as_deref(),
            Some("integrate.api.nvidia.com")
        );
        assert_eq!(
            host_of_url("http://user:pass@Example.com:8080/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of_url("").as_deref(), None);
        assert!(hosts_match("api.openrouter.ai", "openrouter.ai"));
        assert!(!hosts_match("evilopenrouter.ai", "openrouter.ai"));
    }

    #[test]
    fn parses_fixture() {
        let catalog = sample_catalog();
        assert_eq!(catalog.model_count(), 14);
        assert!(Catalog::parse(b"{}").is_err());
        assert!(Catalog::parse(b"[]").is_err());
    }

    #[test]
    fn same_host_match_wins() {
        let catalog = sample_catalog();
        let found = catalog
            .find("https://integrate.api.nvidia.com/v1", "Z-AI/GLM-5.3-Flash")
            .expect("glm is on nvidia");
        assert_eq!(found.kind, MatchKind::SameHost);
        assert_eq!(found.provider.id, "nvidia");
        assert_eq!(found.model.context_tokens, Some(1_000_000));
        assert_eq!(
            found.model.thinking_levels,
            Some(vec![
                "low".to_string(),
                "high".to_string(),
                "max".to_string()
            ])
        );
    }

    #[test]
    fn maker_named_in_the_id() {
        let found = find_in_sample(
            "https://integrate.api.nvidia.com/v1",
            "moonshotai/kimi-k3.1-pro",
        );
        assert_eq!(
            found,
            (
                MatchKind::ModelMaker,
                "moonshotai".to_string(),
                "kimi-k3.1-pro".to_string()
            )
        );
    }

    #[test]
    fn maker_named_by_other_hosts() {
        // No provider is called "z-ai"; nvidia and openrouter both point to zhipuai.
        let glm = find_in_sample("https://example.com/v1", "z-ai/glm-5.3-flash");
        assert_eq!(
            glm,
            (
                MatchKind::ModelMaker,
                "zhipuai".to_string(),
                "glm-5.3-flash".to_string()
            )
        );
        // The openai provider has no API URL, so the maker is found through 302ai.
        let gpt = find_in_sample("https://chatgpt.com/backend-api/codex", "GPT-5.5");
        assert_eq!(
            gpt,
            (
                MatchKind::ModelMaker,
                "openai".to_string(),
                "gpt-5.5".to_string()
            )
        );
    }

    #[test]
    fn other_host_match_is_last() {
        let found = find_in_sample(
            "https://example.com/v1",
            "baidu/ernie-5.5-flash-preview:free",
        );
        assert_eq!(
            found,
            (
                MatchKind::OtherHost,
                "openrouter".to_string(),
                "baidu/ernie-5.5-flash-preview:free".to_string()
            )
        );
    }

    #[test]
    fn no_fuzzy_matching() {
        let catalog = sample_catalog();
        assert!(
            catalog
                .find("https://integrate.api.nvidia.com/v1", "z-ai/glm-5.3")
                .is_none()
        );
    }

    #[test]
    fn effort_levels_follow_cpa_order() {
        let options = serde_json::json!([
            { "type": "effort", "values": ["max", "default", "none", "LOW", "weird"] }
        ]);
        let levels = effort_levels(options.as_array().expect("array"));
        assert_eq!(
            levels,
            Some(vec![
                "none".to_string(),
                "low".to_string(),
                "max".to_string()
            ])
        );
        assert_eq!(effort_levels(&[]), None);
    }

    #[test]
    fn toggle_without_levels_is_marked() {
        let catalog = sample_catalog();
        let found = catalog
            .find(
                "https://openrouter.ai/api/v1",
                "nvidia/nemotron-3.5-super-vl:free",
            )
            .expect("nemotron is on openrouter");
        assert!(found.model.is_thinking_toggle_only);
        assert_eq!(found.model.thinking_levels, None);
    }

    #[test]
    fn reads_status_and_maker_id() {
        let catalog = sample_catalog();
        let found = catalog
            .find("https://integrate.api.nvidia.com/v1", "moonshotai/kimi-k3")
            .expect("kimi-k3 is on nvidia");
        assert_eq!(found.model.status.as_deref(), Some("deprecated"));
        assert_eq!(
            found.model.canonical_model_id.as_deref(),
            Some("moonshotai/kimi-k3")
        );
    }
}
