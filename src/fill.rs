//! Plans which empty model fields models.dev can fill, and writes the ones the
//! user picked. It can also reset a model: remove its saved details, so that
//! CPA uses its defaults again.
//!
//! A field counts as empty when it is missing or empty, when the context
//! window is 0, or when the display name equals the model id. Filling never
//! changes a field that holds a value, and every other part of the config is
//! kept.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog::{Catalog, CatalogMatch, MatchKind, host_of_url};
use crate::error::PluginError;

const OPENAI_COMPATIBILITY: &str = "openai-compatibility";
const CODEX: &str = "codex";
const SECTIONS: [&str; 2] = [OPENAI_COMPATIBILITY, CODEX];

pub const DISPLAY_NAME: &str = "display-name";
pub const CONTEXT_LENGTH: &str = "max-context-length";
pub const INPUT_MODALITIES: &str = "input-modalities";
/// Written to `thinking.levels`; other thinking settings are kept.
pub const THINKING_LEVELS: &str = "thinking.levels";

/// Input types that CPA passes on to clients.
const SUPPORTED_INPUT_MODALITIES: [&str; 2] = ["text", "image"];

/// Where a model sits in the `api-keys` config node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelLocation {
    pub section: &'static str,
    pub entry_index: usize,
    /// Set for models listed under one key, as Codex entries do.
    pub key_index: Option<usize>,
    pub model_index: usize,
}

impl ModelLocation {
    fn json_pointer(&self) -> String {
        match self.key_index {
            Some(key_index) => format!(
                "/{}/{}/keys/{}/models/{}",
                self.section, self.entry_index, key_index, self.model_index
            ),
            None => format!(
                "/{}/{}/models/{}",
                self.section, self.entry_index, self.model_index
            ),
        }
    }

    fn change_id(&self, field: &str) -> String {
        let key_part = self
            .key_index
            .map_or_else(|| "-".to_string(), |key_index| key_index.to_string());
        format!(
            "{}/{}/{}/{}/{}",
            self.section, self.entry_index, key_part, self.model_index, field
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub id: String,
    pub field: &'static str,
    /// The value in the config now; null when the field is missing.
    pub current: Value,
    pub proposed: Value,
    /// Only values from the entry's own host are ticked by default.
    pub is_selected_by_default: bool,
    #[serde(skip)]
    location: ModelLocation,
}

/// What the page shows for one of a model's four details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailState {
    /// The field is empty and models.dev has a value: offered as a change.
    Fill,
    /// The config already holds a value.
    Set,
    /// CPA has no such field for this kind of entry.
    NotUsed,
    /// The field is empty and models.dev has nothing to offer.
    NoData,
}

/// Why models.dev has nothing to offer for an empty field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoDataReason {
    /// The model is not on models.dev.
    NotFound,
    /// models.dev lists the model without this value.
    NotListed,
    /// The models.dev name is the model id itself.
    SameAsId,
    /// The model does not think.
    NoThinking,
    /// Thinking can only be switched on or off.
    OnOffOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldDetail {
    pub field: &'static str,
    pub state: DetailState,
    /// The value in the config now; null when the field is missing.
    pub current: Value,
    /// Set only when the state is `NoData`.
    pub no_data_reason: Option<NoDataReason>,
}

/// Removing a model's saved details, so that CPA uses its defaults again.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelReset {
    pub id: String,
    /// The fields that hold a value now. A reset removes exactly these.
    pub fields: Vec<&'static str>,
    #[serde(skip)]
    location: ModelLocation,
}

/// The models.dev entry a config model was matched to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSource {
    pub match_kind: MatchKind,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: Option<String>,
    /// Shown only; CPA config has no field for these.
    pub has_tool_call: Option<bool>,
    pub output_tokens: Option<u64>,
    pub has_reasoning: bool,
    pub is_thinking_toggle_only: bool,
    pub status: Option<String>,
    pub model_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelPreview {
    pub location: ModelLocation,
    pub entry_name: String,
    pub entry_host: Option<String>,
    pub model_id: String,
    pub source: Option<ModelSource>,
    /// The model's four details, in a fixed order. A detail in the `Fill`
    /// state has its change in `changes`.
    pub details: Vec<FieldDetail>,
    pub changes: Vec<FieldChange>,
    /// Offered when at least one detail holds a value that can be removed.
    pub reset: Option<ModelReset>,
}

/// A change the user picked on the page, with the value the page showed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RequestedChange {
    pub id: String,
    pub proposed: Value,
}

/// A reset the user picked on the page, with the fields the page showed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RequestedReset {
    pub id: String,
    pub fields: Vec<String>,
}

/// Lists every model in the `api-keys` node with the state of its four
/// details and the empty fields that the matched models.dev entry can fill.
/// Only those four field values are copied out, never provider keys.
pub fn plan(api_keys: &Value, catalog: &Catalog) -> Vec<ModelPreview> {
    let mut previews = Vec::new();
    for section in SECTIONS {
        let Some(entries) = api_keys.get(section).and_then(Value::as_array) else {
            continue;
        };
        for (entry_index, entry) in entries.iter().enumerate() {
            let entry_name = trimmed_text(entry, "name").unwrap_or_default();
            let base_url = trimmed_text(entry, "base-url").unwrap_or_default();
            let entry_host = host_of_url(&base_url);
            for (location, model) in entry_models(section, entry_index, entry) {
                let Some(model_id) = trimmed_text(model, "name") else {
                    continue;
                };
                let found = catalog.find(&base_url, &model_id);
                let (details, changes) = model_details(&location, model, &model_id, found.as_ref());
                let reset = model_reset(&location, model, &details);
                previews.push(ModelPreview {
                    location,
                    entry_name: entry_name.clone(),
                    entry_host: entry_host.clone(),
                    model_id,
                    source: found.map(|found| model_source(&found)),
                    details,
                    changes,
                    reset,
                });
            }
        }
    }
    previews
}

/// The planned changes the user picked. Fails when a picked change is no
/// longer planned or would now write a different value, so nothing is saved
/// that the user did not see.
pub fn select(
    previews: &[ModelPreview],
    requested: &[RequestedChange],
) -> Result<Vec<FieldChange>, PluginError> {
    let planned: Vec<&FieldChange> = previews
        .iter()
        .flat_map(|preview| &preview.changes)
        .collect();
    let mut picked: Vec<FieldChange> = Vec::new();
    for request in requested {
        let Some(change) = planned.iter().find(|change| change.id == request.id) else {
            return Err(PluginError::new(
                "preview_outdated",
                "a picked change is no longer offered",
            ));
        };
        if change.proposed != request.proposed {
            return Err(PluginError::new(
                "preview_outdated",
                "a picked change would now save a different value",
            ));
        }
        let is_already_picked = picked.iter().any(|known| known.id == change.id);
        if !is_already_picked {
            picked.push((*change).clone());
        }
    }
    Ok(picked)
}

/// Writes the picked changes into the `api-keys` JSON and returns how many
/// were written.
pub fn apply(api_keys: &mut Value, changes: &[FieldChange]) -> usize {
    let mut applied_count = 0;
    for change in changes {
        let Some(model) = api_keys
            .pointer_mut(&change.location.json_pointer())
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        if change.field == THINKING_LEVELS {
            let thinking = model.entry("thinking").or_insert(Value::Null);
            if !thinking.is_object() {
                *thinking = json!({});
            }
            if let Some(thinking_settings) = thinking.as_object_mut() {
                thinking_settings.insert("levels".to_string(), change.proposed.clone());
                applied_count += 1;
            }
        } else {
            model.insert(change.field.to_string(), change.proposed.clone());
            applied_count += 1;
        }
    }
    applied_count
}

/// The planned resets the user picked. Fails when a picked reset is no
/// longer planned or would now remove other fields than the page showed.
pub fn select_resets(
    previews: &[ModelPreview],
    requested: &[RequestedReset],
) -> Result<Vec<ModelReset>, PluginError> {
    let mut picked: Vec<ModelReset> = Vec::new();
    for request in requested {
        let planned = previews
            .iter()
            .filter_map(|preview| preview.reset.as_ref())
            .find(|reset| reset.id == request.id);
        let Some(reset) = planned else {
            return Err(PluginError::new(
                "preview_outdated",
                "a picked reset is no longer offered",
            ));
        };
        let has_same_fields = reset.fields.len() == request.fields.len()
            && reset
                .fields
                .iter()
                .zip(&request.fields)
                .all(|(planned_field, requested_field)| planned_field == requested_field);
        if !has_same_fields {
            return Err(PluginError::new(
                "preview_outdated",
                "a picked reset would now clear other fields",
            ));
        }
        let is_already_picked = picked.iter().any(|known| known.id == reset.id);
        if !is_already_picked {
            picked.push(reset.clone());
        }
    }
    Ok(picked)
}

/// Removes the listed fields of each picked model and returns how many models
/// changed. The keys are removed, never set to an empty value: an explicit
/// empty value would override CPA's defaults.
pub fn apply_resets(api_keys: &mut Value, resets: &[ModelReset]) -> usize {
    let mut reset_count = 0;
    for reset in resets {
        let Some(model) = api_keys
            .pointer_mut(&reset.location.json_pointer())
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let mut has_removed_a_field = false;
        for field in &reset.fields {
            if *field == THINKING_LEVELS {
                let mut is_thinking_left_empty = false;
                if let Some(thinking) = model.get_mut("thinking").and_then(Value::as_object_mut) {
                    has_removed_a_field |= thinking.shift_remove("levels").is_some();
                    is_thinking_left_empty = thinking.is_empty();
                }
                if is_thinking_left_empty {
                    model.shift_remove("thinking");
                }
            } else {
                has_removed_a_field |= model.shift_remove(*field).is_some();
            }
        }
        if has_removed_a_field {
            reset_count += 1;
        }
    }
    reset_count
}

/// Models listed on the entry itself and under each of its keys.
fn entry_models<'a>(
    section: &'static str,
    entry_index: usize,
    entry: &'a Value,
) -> Vec<(ModelLocation, &'a Value)> {
    let mut models = Vec::new();
    let entry_level_models = entry.get("models").and_then(Value::as_array);
    for (model_index, model) in entry_level_models.into_iter().flatten().enumerate() {
        let location = ModelLocation {
            section,
            entry_index,
            key_index: None,
            model_index,
        };
        models.push((location, model));
    }
    let keys = entry.get("keys").and_then(Value::as_array);
    for (key_index, key) in keys.into_iter().flatten().enumerate() {
        let key_models = key.get("models").and_then(Value::as_array);
        for (model_index, model) in key_models.into_iter().flatten().enumerate() {
            let location = ModelLocation {
                section,
                entry_index,
                key_index: Some(key_index),
                model_index,
            };
            models.push((location, model));
        }
    }
    models
}

/// What models.dev offers for one field.
enum Offer {
    Fill(Value),
    Set,
    NotUsed,
    NoData(NoDataReason),
}

/// The state of each of the four details, with a change for every detail
/// that can be filled. Both come from one pass, so they always agree.
fn model_details(
    location: &ModelLocation,
    model: &Value,
    model_id: &str,
    found: Option<&CatalogMatch<'_>>,
) -> (Vec<FieldDetail>, Vec<FieldChange>) {
    let is_selected_by_default = found.is_some_and(|found| found.kind == MatchKind::SameHost);
    let current_levels = model
        .pointer("/thinking/levels")
        .cloned()
        .unwrap_or(Value::Null);
    let offers = [
        (
            DISPLAY_NAME,
            current_value(model, DISPLAY_NAME),
            display_name_offer(model, model_id, found),
        ),
        (
            CONTEXT_LENGTH,
            current_value(model, CONTEXT_LENGTH),
            context_offer(model, found),
        ),
        (
            INPUT_MODALITIES,
            current_value(model, INPUT_MODALITIES),
            input_modalities_offer(location, model, found),
        ),
        (
            THINKING_LEVELS,
            current_levels,
            thinking_offer(model, found),
        ),
    ];

    let mut details = Vec::new();
    let mut changes = Vec::new();
    for (field, current, offer) in offers {
        let (state, no_data_reason) = match &offer {
            Offer::Fill(_) => (DetailState::Fill, None),
            Offer::Set => (DetailState::Set, None),
            Offer::NotUsed => (DetailState::NotUsed, None),
            Offer::NoData(reason) => (DetailState::NoData, Some(*reason)),
        };
        details.push(FieldDetail {
            field,
            state,
            current: current.clone(),
            no_data_reason,
        });
        if let Offer::Fill(proposed) = offer {
            changes.push(FieldChange {
                id: location.change_id(field),
                field,
                current,
                proposed,
                is_selected_by_default,
                location: location.clone(),
            });
        }
    }
    (details, changes)
}

/// The details of a model that hold a value now, as one reset.
fn model_reset(
    location: &ModelLocation,
    model: &Value,
    details: &[FieldDetail],
) -> Option<ModelReset> {
    let fields: Vec<&'static str> = details
        .iter()
        .filter(|detail| detail.state == DetailState::Set)
        // Thinking set by a token budget alone has no levels to clear.
        .filter(|detail| {
            detail.field != THINKING_LEVELS || !list_is_empty(model.pointer("/thinking/levels"))
        })
        .map(|detail| detail.field)
        .collect();
    if fields.is_empty() {
        return None;
    }
    Some(ModelReset {
        id: location.change_id("reset"),
        fields,
        location: location.clone(),
    })
}

fn display_name_offer(model: &Value, model_id: &str, found: Option<&CatalogMatch<'_>>) -> Offer {
    if !display_name_is_empty(model.get(DISPLAY_NAME), model_id) {
        return Offer::Set;
    }
    let Some(found) = found else {
        return Offer::NoData(NoDataReason::NotFound);
    };
    match &found.model.name {
        None => Offer::NoData(NoDataReason::NotListed),
        // A models.dev name equal to the id would count as empty again next time.
        Some(source_name) if source_name.eq_ignore_ascii_case(model_id) => {
            Offer::NoData(NoDataReason::SameAsId)
        }
        Some(source_name) => Offer::Fill(json!(source_name)),
    }
}

fn context_offer(model: &Value, found: Option<&CatalogMatch<'_>>) -> Offer {
    if !number_is_empty(model.get(CONTEXT_LENGTH)) {
        return Offer::Set;
    }
    let Some(found) = found else {
        return Offer::NoData(NoDataReason::NotFound);
    };
    match found.model.context_tokens {
        Some(context_tokens) => Offer::Fill(json!(context_tokens)),
        None => Offer::NoData(NoDataReason::NotListed),
    }
}

fn input_modalities_offer(
    location: &ModelLocation,
    model: &Value,
    found: Option<&CatalogMatch<'_>>,
) -> Offer {
    // Codex models have no input-modalities field in CPA.
    if location.section != OPENAI_COMPATIBILITY {
        return Offer::NotUsed;
    }
    if !list_is_empty(model.get(INPUT_MODALITIES)) {
        return Offer::Set;
    }
    let Some(found) = found else {
        return Offer::NoData(NoDataReason::NotFound);
    };
    let input_modalities: Vec<&str> = SUPPORTED_INPUT_MODALITIES
        .into_iter()
        .filter(|modality| {
            found
                .model
                .input_modalities
                .iter()
                .any(|source_modality| source_modality == modality)
        })
        .collect();
    if input_modalities.is_empty() {
        Offer::NoData(NoDataReason::NotListed)
    } else {
        Offer::Fill(json!(input_modalities))
    }
}

fn thinking_offer(model: &Value, found: Option<&CatalogMatch<'_>>) -> Offer {
    if !thinking_is_empty(model.get("thinking")) {
        return Offer::Set;
    }
    let Some(found) = found else {
        return Offer::NoData(NoDataReason::NotFound);
    };
    let source_model = found.model;
    if !source_model.reasoning {
        return Offer::NoData(NoDataReason::NoThinking);
    }
    match &source_model.thinking_levels {
        Some(levels) => Offer::Fill(json!(levels)),
        None if source_model.is_thinking_toggle_only => Offer::NoData(NoDataReason::OnOffOnly),
        None => Offer::NoData(NoDataReason::NotListed),
    }
}

fn model_source(found: &CatalogMatch<'_>) -> ModelSource {
    ModelSource {
        match_kind: found.kind,
        provider_id: found.provider.id.clone(),
        provider_name: found.provider.name.clone(),
        model_id: found.model.id.clone(),
        model_name: found.model.name.clone(),
        has_tool_call: found.model.tool_call,
        output_tokens: found.model.output_tokens,
        has_reasoning: found.model.reasoning,
        is_thinking_toggle_only: found.model.is_thinking_toggle_only,
        status: found.model.status.clone(),
        model_type: found.model.model_type.clone(),
    }
}

fn display_name_is_empty(display_name: Option<&Value>, model_id: &str) -> bool {
    match display_name {
        None | Some(Value::Null) => true,
        Some(Value::String(name)) => {
            let trimmed_name = name.trim();
            trimmed_name.is_empty() || trimmed_name.eq_ignore_ascii_case(model_id)
        }
        Some(_) => false,
    }
}

fn number_is_empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Number(number)) => number.as_f64() == Some(0.0),
        Some(_) => false,
    }
}

fn list_is_empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(_) => false,
    }
}

/// Thinking counts as empty when it has no levels and no token budget.
fn thinking_is_empty(thinking: Option<&Value>) -> bool {
    match thinking {
        None | Some(Value::Null) => true,
        Some(Value::Object(thinking_settings)) => {
            let has_levels = !list_is_empty(thinking_settings.get("levels"));
            let has_budget = ["min", "max"]
                .into_iter()
                .any(|name| !number_is_empty(thinking_settings.get(name)));
            !has_levels && !has_budget
        }
        Some(_) => false,
    }
}

fn current_value(model: &Value, field: &str) -> Value {
    model.get(field).cloned().unwrap_or(Value::Null)
}

fn trimmed_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_catalog() -> Catalog {
        let bytes = include_bytes!("../tests/fixtures/models-dev-sample.json");
        Catalog::parse(bytes).expect("models.dev fixture should parse")
    }

    fn sample_api_keys() -> Value {
        let bytes = include_bytes!("../tests/fixtures/api-keys-sample.json");
        serde_json::from_slice(bytes).expect("api-keys fixture should parse")
    }

    fn preview_for<'a>(previews: &'a [ModelPreview], model_id: &str) -> &'a ModelPreview {
        previews
            .iter()
            .find(|preview| preview.model_id == model_id)
            .expect("the model should be in the preview")
    }

    fn change_for<'a>(preview: &'a ModelPreview, field: &str) -> &'a FieldChange {
        preview
            .changes
            .iter()
            .find(|change| change.field == field)
            .expect("the field should have a change")
    }

    fn fields_of(preview: &ModelPreview) -> Vec<&'static str> {
        preview.changes.iter().map(|change| change.field).collect()
    }

    fn all_changes(previews: &[ModelPreview]) -> Vec<FieldChange> {
        previews
            .iter()
            .flat_map(|preview| preview.changes.iter().cloned())
            .collect()
    }

    #[test]
    fn same_host_models_get_every_field_ticked() {
        let previews = plan(&sample_api_keys(), &sample_catalog());
        assert_eq!(previews.len(), 8);

        let glm = preview_for(&previews, "z-ai/glm-5.3-flash");
        let source = glm.source.as_ref().expect("glm is on nvidia");
        assert_eq!(source.match_kind, MatchKind::SameHost);
        assert_eq!(source.provider_id, "nvidia");
        assert_eq!(
            fields_of(glm),
            [
                DISPLAY_NAME,
                CONTEXT_LENGTH,
                INPUT_MODALITIES,
                THINKING_LEVELS
            ]
        );
        assert!(
            glm.changes
                .iter()
                .all(|change| change.is_selected_by_default)
        );
        assert_eq!(
            change_for(glm, DISPLAY_NAME).current,
            json!("z-ai/glm-5.3-flash")
        );
        assert_eq!(
            change_for(glm, DISPLAY_NAME).proposed,
            json!("GLM-5.3-Flash")
        );
        assert_eq!(change_for(glm, CONTEXT_LENGTH).proposed, json!(1_000_000));
        assert_eq!(
            change_for(glm, INPUT_MODALITIES).proposed,
            json!(["text", "image"])
        );
        assert_eq!(
            change_for(glm, THINKING_LEVELS).proposed,
            json!(["low", "high", "max"])
        );
        assert_eq!(
            change_for(glm, DISPLAY_NAME).id,
            "openai-compatibility/0/-/3/display-name"
        );

        let step = preview_for(&previews, "stepfun-ai/step-5-preview");
        assert_eq!(
            change_for(step, THINKING_LEVELS).proposed,
            json!(["none", "minimal", "low", "medium", "high", "xhigh", "max"])
        );
    }

    #[test]
    fn maker_values_are_offered_but_not_ticked() {
        let previews = plan(&sample_api_keys(), &sample_catalog());
        let kimi = preview_for(&previews, "moonshotai/kimi-k3.1-pro");
        let source = kimi.source.as_ref().expect("kimi is listed by its maker");
        assert_eq!(source.match_kind, MatchKind::ModelMaker);
        assert_eq!(source.provider_id, "moonshotai");
        assert_eq!(source.model_id, "kimi-k3.1-pro");
        assert!(!kimi.changes.is_empty());
        assert!(
            kimi.changes
                .iter()
                .all(|change| !change.is_selected_by_default)
        );
    }

    #[test]
    fn codex_models_follow_their_own_rules() {
        let previews = plan(&sample_api_keys(), &sample_catalog());

        let ernie = preview_for(&previews, "baidu/ernie-5.5-flash-preview:free");
        assert_eq!(
            fields_of(ernie),
            [DISPLAY_NAME, CONTEXT_LENGTH, THINKING_LEVELS]
        );
        assert_eq!(change_for(ernie, THINKING_LEVELS).proposed, json!(["high"]));
        assert_eq!(
            change_for(ernie, DISPLAY_NAME).id,
            "codex/0/0/0/display-name"
        );

        let nemotron = preview_for(&previews, "nvidia/nemotron-3.5-super-vl:free");
        assert_eq!(fields_of(nemotron), [DISPLAY_NAME, CONTEXT_LENGTH]);
        let nemotron_source = nemotron.source.as_ref().expect("nemotron is on openrouter");
        assert!(nemotron_source.is_thinking_toggle_only);

        let free_router = preview_for(&previews, "openrouter/free");
        assert_eq!(fields_of(free_router), [DISPLAY_NAME, CONTEXT_LENGTH]);

        let particle = preview_for(&previews, "particle-ai/particle-1:free");
        assert!(particle.source.is_none());
        assert!(particle.changes.is_empty());
    }

    fn detail_for<'a>(preview: &'a ModelPreview, field: &str) -> &'a FieldDetail {
        preview
            .details
            .iter()
            .find(|detail| detail.field == field)
            .expect("every model lists the field")
    }

    #[test]
    fn details_agree_with_changes() {
        let mut api_keys = sample_api_keys();
        api_keys["openai-compatibility"][0]["models"][3]["display-name"] = json!("My GLM");
        let previews = plan(&api_keys, &sample_catalog());
        for preview in &previews {
            let detail_fields: Vec<&str> =
                preview.details.iter().map(|detail| detail.field).collect();
            assert_eq!(
                detail_fields,
                [
                    DISPLAY_NAME,
                    CONTEXT_LENGTH,
                    INPUT_MODALITIES,
                    THINKING_LEVELS
                ]
            );
            let fillable_fields: Vec<&str> = preview
                .details
                .iter()
                .filter(|detail| detail.state == DetailState::Fill)
                .map(|detail| detail.field)
                .collect();
            assert_eq!(fillable_fields, fields_of(preview), "{}", preview.model_id);
            for detail in &preview.details {
                let has_reason = detail.no_data_reason.is_some();
                assert_eq!(has_reason, detail.state == DetailState::NoData);
            }
        }
    }

    #[test]
    fn details_say_why_nothing_is_offered() {
        let mut api_keys = sample_api_keys();
        let glm = api_keys
            .pointer_mut("/openai-compatibility/0/models/3")
            .expect("glm is the fourth model");
        glm["display-name"] = json!("My GLM");
        glm["thinking"] = json!({ "levels": ["high"] });
        let previews = plan(&api_keys, &sample_catalog());

        let glm_preview = preview_for(&previews, "z-ai/glm-5.3-flash");
        let glm_name = detail_for(glm_preview, DISPLAY_NAME);
        assert_eq!(glm_name.state, DetailState::Set);
        assert_eq!(glm_name.current, json!("My GLM"));
        let glm_levels = detail_for(glm_preview, THINKING_LEVELS);
        assert_eq!(glm_levels.state, DetailState::Set);
        assert_eq!(glm_levels.current, json!(["high"]));
        assert_eq!(
            detail_for(glm_preview, CONTEXT_LENGTH).state,
            DetailState::Fill
        );

        let state_of = |model_id: &str, field: &str| {
            let detail = detail_for(preview_for(&previews, model_id), field);
            (detail.state, detail.no_data_reason)
        };
        assert_eq!(
            state_of("baidu/ernie-5.5-flash-preview:free", INPUT_MODALITIES),
            (DetailState::NotUsed, None)
        );
        assert_eq!(
            state_of("nvidia/nemotron-3.5-super-vl:free", THINKING_LEVELS),
            (DetailState::NoData, Some(NoDataReason::OnOffOnly))
        );
        assert_eq!(
            state_of("openrouter/free", THINKING_LEVELS),
            (DetailState::NoData, Some(NoDataReason::NotListed))
        );
        assert_eq!(
            state_of("particle-ai/particle-1:free", DISPLAY_NAME),
            (DetailState::NoData, Some(NoDataReason::NotFound))
        );
        assert_eq!(
            state_of("particle-ai/particle-1:free", INPUT_MODALITIES),
            (DetailState::NotUsed, None)
        );
    }

    fn reset_request(preview: &ModelPreview) -> RequestedReset {
        let reset = preview.reset.as_ref().expect("the model can be reset");
        RequestedReset {
            id: reset.id.clone(),
            fields: reset.fields.iter().map(|field| field.to_string()).collect(),
        }
    }

    #[test]
    fn reset_is_offered_only_for_saved_details() {
        let catalog = sample_catalog();
        let mut api_keys = sample_api_keys();
        let previews = plan(&api_keys, &catalog);
        // A display name equal to the model id is not a saved detail.
        assert!(previews.iter().all(|preview| preview.reset.is_none()));

        let changes = all_changes(&previews);
        apply(&mut api_keys, &changes);
        let previews = plan(&api_keys, &catalog);
        let fields_of_reset = |model_id: &str| {
            let reset = preview_for(&previews, model_id).reset.as_ref();
            reset.map(|reset| reset.fields.clone())
        };
        assert_eq!(
            fields_of_reset("z-ai/glm-5.3-flash"),
            Some(vec![
                DISPLAY_NAME,
                CONTEXT_LENGTH,
                INPUT_MODALITIES,
                THINKING_LEVELS
            ])
        );
        assert_eq!(
            fields_of_reset("nvidia/nemotron-3.5-super-vl:free"),
            Some(vec![DISPLAY_NAME, CONTEXT_LENGTH])
        );
        assert_eq!(fields_of_reset("particle-ai/particle-1:free"), None);
        assert_eq!(
            preview_for(&previews, "z-ai/glm-5.3-flash")
                .reset
                .as_ref()
                .map(|reset| reset.id.as_str()),
            Some("openai-compatibility/0/-/3/reset")
        );
    }

    #[test]
    fn filling_then_resetting_gives_the_original_back() {
        let catalog = sample_catalog();
        let original = sample_api_keys();
        let mut api_keys = original.clone();
        let codex_changes: Vec<FieldChange> = all_changes(&plan(&api_keys, &catalog))
            .into_iter()
            .filter(|change| change.id.starts_with("codex/"))
            .collect();
        assert_eq!(apply(&mut api_keys, &codex_changes), 7);
        assert_ne!(api_keys, original);

        let previews = plan(&api_keys, &catalog);
        let requested: Vec<RequestedReset> = previews
            .iter()
            .filter(|preview| preview.location.section == "codex" && preview.reset.is_some())
            .map(reset_request)
            .collect();
        assert_eq!(requested.len(), 3);
        let resets = select_resets(&previews, &requested).expect("the resets were offered");
        assert_eq!(apply_resets(&mut api_keys, &resets), 3);
        // The same JSON text: the same keys in the same order, nothing left behind.
        assert_eq!(
            serde_json::to_string(&api_keys).expect("JSON text"),
            serde_json::to_string(&original).expect("JSON text")
        );
    }

    #[test]
    fn reset_keeps_every_other_setting() {
        let catalog = sample_catalog();
        let mut api_keys = sample_api_keys();
        api_keys["openai-compatibility"][0]["models"][3] = json!({
            "display-name": "My GLM",
            "name": "z-ai/glm-5.3-flash",
            "alias": "glm",
            "thinking": { "levels": ["high"], "zero-allowed": true },
            "max-context-length": 5000
        });
        let previews = plan(&api_keys, &catalog);
        let glm = preview_for(&previews, "z-ai/glm-5.3-flash");
        assert_eq!(
            glm.reset.as_ref().expect("glm has saved details").fields,
            [DISPLAY_NAME, CONTEXT_LENGTH, THINKING_LEVELS]
        );
        let resets = select_resets(&previews, &[reset_request(glm)]).expect("offered");
        assert_eq!(apply_resets(&mut api_keys, &resets), 1);
        assert_eq!(
            serde_json::to_string(&api_keys["openai-compatibility"][0]["models"][3])
                .expect("JSON text"),
            r#"{"name":"z-ai/glm-5.3-flash","alias":"glm","thinking":{"zero-allowed":true}}"#
        );
        // The other models are untouched.
        assert_eq!(
            api_keys["openai-compatibility"][0]["models"][2],
            sample_api_keys()["openai-compatibility"][0]["models"][2]
        );
    }

    #[test]
    fn a_thinking_budget_is_not_cleared() {
        let catalog = sample_catalog();
        let mut api_keys = sample_api_keys();
        api_keys["openai-compatibility"][0]["models"][3]["thinking"] =
            json!({ "min": 1024, "max": 8192 });
        let previews = plan(&api_keys, &catalog);
        let glm = preview_for(&previews, "z-ai/glm-5.3-flash");
        assert_eq!(detail_for(glm, THINKING_LEVELS).state, DetailState::Set);
        assert_eq!(glm.reset, None);

        api_keys["openai-compatibility"][0]["models"][3]["display-name"] = json!("My GLM");
        let previews = plan(&api_keys, &catalog);
        let glm = preview_for(&previews, "z-ai/glm-5.3-flash");
        assert_eq!(
            glm.reset.as_ref().expect("the name can be cleared").fields,
            [DISPLAY_NAME]
        );
    }

    #[test]
    fn select_resets_refuses_what_the_user_did_not_see() {
        let catalog = sample_catalog();
        let mut api_keys = sample_api_keys();
        let changes = all_changes(&plan(&api_keys, &catalog));
        apply(&mut api_keys, &changes);
        let previews = plan(&api_keys, &catalog);
        let seen = reset_request(preview_for(&previews, "z-ai/glm-5.3-flash"));

        let picked = select_resets(&previews, &[seen.clone(), seen.clone()]).expect("offered");
        assert_eq!(picked.len(), 1);

        let fewer_fields = RequestedReset {
            fields: seen.fields[..2].to_vec(),
            ..seen.clone()
        };
        let error = select_resets(&previews, &[fewer_fields]).expect_err("fields differ");
        assert_eq!(error.code, "preview_outdated");

        let unknown_model = RequestedReset {
            id: "codex/9/-/9/reset".to_string(),
            ..seen
        };
        let error = select_resets(&previews, &[unknown_model]).expect_err("id is unknown");
        assert_eq!(error.code, "preview_outdated");
    }

    #[test]
    fn filled_fields_are_kept() {
        let mut api_keys = sample_api_keys();
        let glm = api_keys
            .pointer_mut("/openai-compatibility/0/models/3")
            .expect("glm is the fourth model");
        glm["display-name"] = json!("My GLM");
        glm["max-context-length"] = json!(0);
        glm["input-modalities"] = json!(["text"]);
        glm["thinking"] = json!({ "zero-allowed": true });
        let step = api_keys
            .pointer_mut("/openai-compatibility/0/models/2")
            .expect("step is the third model");
        step["display-name"] = json!("STEPFUN-AI/STEP-5-PREVIEW");
        step["thinking"] = json!({ "levels": ["high"] });

        let previews = plan(&api_keys, &sample_catalog());
        let glm_preview = preview_for(&previews, "z-ai/glm-5.3-flash");
        assert_eq!(fields_of(glm_preview), [CONTEXT_LENGTH, THINKING_LEVELS]);
        let step_preview = preview_for(&previews, "stepfun-ai/step-5-preview");
        assert_eq!(
            fields_of(step_preview),
            [DISPLAY_NAME, CONTEXT_LENGTH, INPUT_MODALITIES]
        );
    }

    #[test]
    fn thinking_levels_join_other_thinking_settings() {
        let mut api_keys = sample_api_keys();
        api_keys["openai-compatibility"][0]["models"][3]["thinking"] =
            json!({ "zero-allowed": true });
        let previews = plan(&api_keys, &sample_catalog());
        let levels_change = change_for(
            preview_for(&previews, "z-ai/glm-5.3-flash"),
            THINKING_LEVELS,
        );
        apply(&mut api_keys, std::slice::from_ref(levels_change));
        assert_eq!(
            api_keys["openai-compatibility"][0]["models"][3]["thinking"],
            json!({ "zero-allowed": true, "levels": ["low", "high", "max"] })
        );
    }

    #[test]
    fn applying_everything_leaves_nothing_to_fill() {
        let catalog = sample_catalog();
        let mut api_keys = sample_api_keys();
        let changes = all_changes(&plan(&api_keys, &catalog));
        assert_eq!(apply(&mut api_keys, &changes), changes.len());
        let previews_after = plan(&api_keys, &catalog);
        assert!(
            previews_after
                .iter()
                .all(|preview| preview.changes.is_empty())
        );
    }

    #[test]
    fn apply_touches_only_the_picked_fields() {
        let catalog = sample_catalog();
        let original = sample_api_keys();
        let mut updated = original.clone();
        let picked: Vec<FieldChange> = all_changes(&plan(&original, &catalog))
            .into_iter()
            .filter(|change| change.is_selected_by_default)
            .collect();
        apply(&mut updated, &picked);

        // The replaced display name stays in its place; new fields go last.
        let glm_keys: Vec<&String> = updated["openai-compatibility"][0]["models"][3]
            .as_object()
            .expect("glm is an object")
            .keys()
            .collect();
        assert_eq!(
            glm_keys,
            [
                "display-name",
                "name",
                "max-context-length",
                "input-modalities",
                "thinking"
            ]
        );

        // With the picked fields removed from both, the JSON text is the same,
        // so keys, auth_index values and the order of everything else are kept.
        let without_picked_fields = |api_keys: &Value| {
            let mut copy = api_keys.clone();
            for change in &picked {
                let model = copy
                    .pointer_mut(&change.location.json_pointer())
                    .and_then(Value::as_object_mut)
                    .expect("the model exists");
                let config_field = if change.field == THINKING_LEVELS {
                    "thinking"
                } else {
                    change.field
                };
                model.shift_remove(config_field);
            }
            serde_json::to_string(&copy).expect("JSON text")
        };
        assert_eq!(
            without_picked_fields(&updated),
            without_picked_fields(&original)
        );
    }

    #[test]
    fn preview_never_contains_provider_keys() {
        let previews = plan(&sample_api_keys(), &sample_catalog());
        let preview_text = serde_json::to_string(&previews).expect("JSON text");
        assert!(!preview_text.contains("sentinel-"));
        assert!(!preview_text.contains("api-key"));
    }

    #[test]
    fn select_refuses_changes_the_user_did_not_see() {
        let previews = plan(&sample_api_keys(), &sample_catalog());
        let name_change = change_for(preview_for(&previews, "z-ai/glm-5.3-flash"), DISPLAY_NAME);
        let seen_change = RequestedChange {
            id: name_change.id.clone(),
            proposed: name_change.proposed.clone(),
        };

        let picked = select(&previews, &[seen_change.clone(), seen_change.clone()])
            .expect("a change from the preview is accepted");
        assert_eq!(picked.len(), 1);

        let different_value = RequestedChange {
            proposed: json!("Something else"),
            ..seen_change
        };
        let error = select(&previews, &[different_value]).expect_err("value differs");
        assert_eq!(error.code, "preview_outdated");

        let unknown_change = RequestedChange {
            id: "codex/9/-/9/display-name".to_string(),
            proposed: json!("x"),
        };
        let error = select(&previews, &[unknown_change]).expect_err("id is unknown");
        assert_eq!(error.code, "preview_outdated");
    }
}
