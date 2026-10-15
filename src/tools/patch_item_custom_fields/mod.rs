/*
 * Copyright 2026 Mia srl
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
// `patch_item_custom_fields`: the only way to write an item's custom fields, since a `PUT` ignores
// them. One `PATCH`, which the engine merges and guards itself; an empty patch is discovery
// instead, never a write; and a refused patch comes back explained key by key.

use crate::{
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::{
        apply_item::{MAX_KIND_BYTES, MAX_NAME_BYTES},
        arguments::validate_group,
    },
};
use catalog_client::{
    EngineClient, ItemAddress, Remedy, ToolError, TypeCoordinates, error::codes, is_valid_kind,
    is_valid_name, resolve_kind_or_suggest,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The tenant's custom-field definitions, and what each key of a refused patch is.
mod discovery;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "patch_item_custom_fields";

/// What the tool does. Discovery first, because a guessed key is a write on a shared tenant;
/// then the merge rule, the one difference from `apply_item`, and what comes back.
const TOOL_DESCRIPTION: &str = "Sets or removes custom fields on a catalog item (`name`, `kind`, \
     and `group` for a shared kind). Send empty `fields` first: it changes nothing and returns the \
     item's `customFields` and `validKeys`, the keys it can take with their schemas. Then send \
     only the keys to change: a value sets the key, `null` removes it, keys left out are kept. A \
     value is replaced whole. On a wrong key nothing is written and the error says why. Returns \
     `customFields` after the write, and the keys `set` and `removed` as requested.";

/// The most keys one call may carry.
pub const MAX_FIELDS: usize = 50;

/// The longest key, in bytes. Input hygiene, not the engine's limit: its key pattern is not
/// anchored at the end, so a key stored today may be longer than the pattern suggests, and every
/// stored key must stay writable and removable.
pub const MAX_KEY_BYTES: usize = 1024;

/// Said with every discovery answer.
const DISCOVERY_WARNING: &str = "Nothing was written. Whether this item can be written is checked \
     only when you send keys.";

/// Said when the definitions could not be listed — most often a caller allowed to write the item
/// but not to read the custom-field definitions.
const DEFINITIONS_UNREADABLE: &str = "The custom-field definitions could not be read, so the keys \
     could not be checked.";

/// The next step after a `404`: the engine checks the keys before the item, so a missing item
/// is often a wrong `kind`.
const NOT_FOUND_NEXT_STEP: &str = "call describe_item with this `name` to find the item and its \
     kind";

/// The next step after a write whose outcome is unknown. The patch is idempotent, so one resend is
/// safe; more is not useful, since some engine failures repeat on every attempt.
const UNKNOWN_OUTCOME_NEXT_STEP: &str = "read the item with describe_item; if the values are not \
     there, send the same patch once more; if that fails too, escalate";

/// Arguments for `patch_item_custom_fields`.
///
/// `fields` holds raw values so that a JSON `null` arrives as `Value::Null`, the removal, rather
/// than collapsing into "not mentioned"; a `BTreeMap` keeps the body and the answer in key order.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[serde(deny_unknown_fields)]
pub struct PatchItemCustomFieldsInput {
    /// The item's name.
    #[serde(rename = "name")]
    pub name: String,

    /// The item's type.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// Keys to change: a value sets, `null` removes, a key left out is kept. Empty to discover.
    #[serde(rename = "fields")]
    pub fields: BTreeMap<String, Value>,
}

/// What a write did: the map as the engine now holds it, and the keys as requested.
#[derive(Serialize)]
struct Patched {
    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "version")]
    version: String,

    #[serde(rename = "family")]
    family: String,

    /// The keys sent with a value. **Present and empty** when there were none.
    #[serde(rename = "set")]
    set: Vec<String>,

    /// The keys sent as `null`, whether or not the item had them.
    #[serde(rename = "removed")]
    removed: Vec<String>,

    /// The item's custom fields after the write: the truth `set` and `removed` only ask for.
    #[serde(rename = "customFields")]
    custom_fields: Value,
}

/// What discovery found: the map, and the keys the item's type can take.
#[derive(Serialize)]
struct Discovered {
    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "version")]
    version: String,

    #[serde(rename = "family")]
    family: String,

    #[serde(rename = "customFields")]
    custom_fields: Value,

    /// Absent when the definitions could not be read.
    #[serde(rename = "validKeys", skip_serializing_if = "Option::is_none")]
    valid_keys: Option<Map<String, Value>>,
}

/// `patch_item_custom_fields` — set or remove an item's custom fields, or discover them.
///
/// **No read-merge-write here**: the engine merges a custom-fields patch onto what it reads itself
/// and re-merges when another write lands in between, so the tool sends the caller's keys and
/// nothing else.
pub struct PatchItemCustomFields;

impl Tool for PatchItemCustomFields {
    type Input = PatchItemCustomFieldsInput;

    /// Not destructive, idempotent: the same patch twice reaches the same state.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<PatchItemCustomFieldsInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().destructive(false).idempotent(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;

        let engine = context.engine();

        // Never guess the type of a write, as `apply_item`.
        let coordinates =
            resolve_kind_or_suggest(engine, &input.kind, input.group.as_deref()).await?;
        let address = ItemAddress::new(
            &coordinates.group,
            &coordinates.version,
            &coordinates.family,
            &input.name,
        )?;

        // An empty patch would still be written and announced to every webhook subscriber.
        if input.fields.is_empty() {
            return discover(engine, &coordinates, &address).await;
        }

        let body = Value::Object(
            input
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );

        let item = match engine.patch_item_custom_fields(&address, &body).await {
            Ok(response) => response.value,
            Err(error) => return Err(on_refusal(engine, &coordinates, &input.fields, error).await),
        };

        let set = keys_where(&input.fields, |value| !value.is_null());
        let removed = keys_where(&input.fields, Value::is_null);

        render(Patched {
            name: input.name,
            kind: coordinates.kind,
            group: coordinates.group,
            version: coordinates.version,
            family: coordinates.family,
            set,
            removed,
            custom_fields: item.custom_fields.unwrap_or_else(|| json!({})),
        })
    }
}

/// Discovery: the item's map and the keys its type can take. Two reads, no write.
async fn discover(
    engine: &EngineClient,
    coordinates: &TypeCoordinates,
    address: &ItemAddress,
) -> Result<ToolOutput, ToolError> {
    let item = engine
        .get_item(address)
        .await
        .map_err(not_found_hint)?
        .value;

    let (valid_keys, unreadable) = match discovery::list_definitions(engine).await {
        Ok(definitions) => (
            Some(discovery::valid_keys(
                &definitions,
                &discovery::type_urn(coordinates),
                None,
            )),
            false,
        ),
        Err(error) => {
            tracing::warn!(%error, "the custom-field definitions could not be listed");
            (None, true)
        }
    };

    let output = render(Discovered {
        name: address.name().to_string(),
        kind: coordinates.kind.clone(),
        group: coordinates.group.clone(),
        version: coordinates.version.clone(),
        family: coordinates.family.clone(),
        custom_fields: item.custom_fields.unwrap_or_else(|| json!({})),
        valid_keys,
    })?
    .with_warning(DISCOVERY_WARNING);

    Ok(if unreadable {
        output.with_warning(DEFINITIONS_UNREADABLE)
    } else {
        output
    })
}

/// What a refused write tells the model.
///
/// A `400` is the caller's input — the client sends the patch as such — and is explained key by
/// key; a `404` points at the kind; an unknown outcome says how to find out. Anything else passes
/// through as the client mapped it.
async fn on_refusal(
    engine: &EngineClient,
    coordinates: &TypeCoordinates,
    fields: &BTreeMap<String, Value>,
    error: ToolError,
) -> ToolError {
    match error.code {
        codes::INVALID_INPUT => explain_bad_request(engine, coordinates, fields, error).await,
        codes::NOT_FOUND => not_found_hint(error),
        codes::UNKNOWN_OUTCOME => error.with_next_step(UNKNOWN_OUTCOME_NEXT_STEP),
        codes::SERVER_DEFECT => {
            tracing::error!(%error, "the catalog refused a custom-fields patch this server built");
            error
        }
        _ => error,
    }
}

/// The engine's `400`, explained.
///
/// The keys given a value are classified against every definition. When some key is wrong, the
/// answer names each one, and the remedy follows what is left to write: something writable, or a
/// key the model can correct, is `RetryAfterChange`; only fields meant for other types or for none
/// is `unsupported_for_type`, because a person must change the definitions. When every key is
/// valid, or none was given a value, the engine's own message is the explanation. Nothing was
/// written in any case, and the engine's message is always kept.
async fn explain_bad_request(
    engine: &EngineClient,
    coordinates: &TypeCoordinates,
    fields: &BTreeMap<String, Value>,
    error: ToolError,
) -> ToolError {
    let engine_message = error.message.clone();
    let given: Vec<&str> = fields
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, _)| key.as_str())
        .collect();

    // `null` keys are never checked by the engine: the refusal is about something else.
    if given.is_empty() {
        return relayed(error, &engine_message, None);
    }

    let definitions = match discovery::list_definitions(engine).await {
        Ok(definitions) => definitions,
        Err(listing_error) => {
            tracing::warn!(error = %listing_error, "the custom-field definitions could not be listed");
            return relayed(error, &engine_message, Some(DEFINITIONS_UNREADABLE));
        }
    };

    let type_urn = discovery::type_urn(coordinates);
    let classes: BTreeMap<String, discovery::KeyClass> = given
        .iter()
        .map(|key| {
            (
                (*key).to_string(),
                discovery::classify(&definitions, key, &type_urn),
            )
        })
        .collect();

    let reasons: Vec<String> = classes
        .iter()
        .filter_map(|(key, class)| discovery::explain(key, class, coordinates))
        .collect();

    // Every key applies: the engine refused a value, or something the keys do not explain.
    if reasons.is_empty() {
        let details = merged_details(
            &error,
            [
                ("engineMessage", json!(engine_message)),
                (
                    "validKeys",
                    Value::Object(discovery::valid_keys(&definitions, &type_urn, Some(&given))),
                ),
            ],
        );

        return ToolError::new(
            codes::INVALID_INPUT,
            Remedy::RetryAfterChange,
            format!("Nothing was written. {engine_message}"),
        )
        .with_details(details);
    }

    let details = merged_details(
        &error,
        [
            ("keys", discovery::keys_json(&classes)),
            (
                "validKeys",
                Value::Object(discovery::valid_keys(&definitions, &type_urn, None)),
            ),
            ("engineMessage", json!(engine_message)),
        ],
    );

    let anything_writable = classes.values().any(discovery::KeyClass::is_writable);
    let refused = if anything_writable {
        ToolError::new(
            codes::INVALID_INPUT,
            Remedy::RetryAfterChange,
            format!(
                "Nothing was written. {} Correct or drop those keys and send the patch again; \
                 `validKeys` lists the keys this item can take.",
                reasons.join(" ")
            ),
        )
    } else {
        ToolError::new(
            codes::UNSUPPORTED_FOR_TYPE,
            Remedy::Escalate,
            format!(
                "Nothing was written. {} A person must change these fields' definitions before \
                 this item can carry them.",
                reasons.join(" ")
            ),
        )
    };

    refused.with_details(details)
}

/// The engine's refusal as it is, saying that nothing was written, with its message kept in the
/// details beside what the client put there.
fn relayed(error: ToolError, engine_message: &str, note: Option<&str>) -> ToolError {
    let details = merged_details(&error, [("engineMessage", json!(engine_message))]);
    let message = match note {
        Some(note) => format!("Nothing was written. {engine_message} {note}"),
        None => format!("Nothing was written. {engine_message}"),
    };

    ToolError {
        message,
        details: Some(Box::new(details)),
        ..error
    }
}

/// The error's own details — its `requestId`, say — with `entries` added.
fn merged_details<const N: usize>(error: &ToolError, entries: [(&str, Value); N]) -> Value {
    let mut details = error
        .details
        .as_deref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    for (key, value) in entries {
        details.insert(key.to_string(), value);
    }

    Value::Object(details)
}

/// A `404` with the call that finds the item: the engine checks keys before the item, so a missing
/// item is most often a wrong `kind`.
fn not_found_hint(error: ToolError) -> ToolError {
    if error.code == codes::NOT_FOUND {
        error.with_next_step(NOT_FOUND_NEXT_STEP)
    } else {
        error
    }
}

/// The keys of `fields` whose value satisfies `keep`, in key order.
fn keys_where(fields: &BTreeMap<String, Value>, keep: impl Fn(&Value) -> bool) -> Vec<String> {
    fields
        .iter()
        .filter(|(_, value)| keep(value))
        .map(|(key, _)| key.clone())
        .collect()
}

/// The answer, as the payload the runtime renders.
fn render(answer: impl Serialize) -> Result<ToolOutput, ToolError> {
    serde_json::to_value(answer)
        .map(ToolOutput::new)
        .map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The answer could not be rendered: {err}"),
            )
        })
}

/// The input bounds, checked before anything reaches the engine — `name`, `kind` and `group` as
/// `apply_item` checks them.
fn validate(input: &PatchItemCustomFieldsInput) -> Result<(), ToolError> {
    if input.name.is_empty() || input.name.len() > MAX_NAME_BYTES {
        return Err(invalid(
            "name",
            format!("`name` must be between 1 and {MAX_NAME_BYTES} bytes."),
        ));
    }

    if !is_valid_name(&input.name) {
        return Err(invalid(
            "name",
            format!(
                "`{}` is not an item name: lowercase letters, digits, `-` and `.`, starting and \
                 ending with a letter or digit.",
                input.name
            ),
        ));
    }

    if input.kind.is_empty() || input.kind.len() > MAX_KIND_BYTES {
        return Err(invalid(
            "kind",
            format!("`kind` must be between 1 and {MAX_KIND_BYTES} bytes."),
        ));
    }

    if !is_valid_kind(&input.kind) {
        return Err(invalid(
            "kind",
            format!(
                "`{}` is not a kind: a kind is a letter followed by letters and digits.",
                input.kind
            ),
        )
        .with_next_step("call list_catalog_types to see the kinds that exist"));
    }

    validate_group(input.group.as_deref(), true)?;

    if input.fields.len() > MAX_FIELDS {
        return Err(invalid(
            "fields",
            format!(
                "`fields` has {} keys and one call takes at most {MAX_FIELDS}.",
                input.fields.len()
            ),
        ));
    }

    if let Some(key) = input
        .fields
        .keys()
        .find(|key| key.is_empty() || key.len() > MAX_KEY_BYTES)
    {
        return Err(invalid(
            "fields",
            format!(
                "Every key in `fields` must be between 1 and {MAX_KEY_BYTES} bytes; `{}` is {} \
                 bytes.",
                key.chars().take(32).collect::<String>(),
                key.len()
            ),
        ));
    }

    Ok(())
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

#[cfg(test)]
mod tests;
