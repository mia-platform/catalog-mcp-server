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
// The tool's input, turned into the RFC 7396 document the core's write cycle merges, and the
// `customFields` check.

use crate::tools::apply_item::{ApplyItemInput, ItemMetadataPatch};
use catalog_client::{ItemAddress, Remedy, ToolError, error::codes};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

/// The key custom fields sit under, which a `PUT` ignores.
pub(super) const CUSTOM_FIELDS_KEY: &str = "customFields";

/// Deserialises a field that is **present**, `null` included, as `Some`.
///
/// Paired with `#[serde(default)]`, an absent field stays `None` while `null` becomes
/// `Some(Value::Null)`. Serde's own `Option` handling maps `null` to `None`, which would turn
/// *"remove the title"* into *"do not mention the title"* — the one distinction RFC 7396 exists to
/// make.
pub(crate) fn present<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// Removes a schema's `description`, for a type whose doc comment is written for this code's
/// readers rather than the model: `tools/list` would pay for it on every conversation.
pub(super) fn without_description(schema: &mut schemars::Schema) {
    schema.remove("description");
}

/// The merge patch for `input`, addressed at `address`.
///
/// Besides what the caller sent, it carries `apiVersion`, `kind` and `metadata.name`. On an update
/// they equal what is stored, so they change nothing; on a create they are what the engine
/// requires of a new item's body, which the caller's fields alone would not make.
///
/// # Errors
///
/// `invalid_input` when `spec` is not an object, or names `customFields`.
pub(super) fn document(input: &ApplyItemInput, address: &ItemAddress) -> Result<Value, ToolError> {
    let mut patch = Map::new();
    patch.insert("apiVersion".into(), json!(address.api_version()));
    patch.insert("kind".into(), json!(input.kind));

    let mut metadata = input
        .metadata
        .as_ref()
        .map(metadata_patch)
        .unwrap_or_default();
    metadata.insert("name".into(), json!(address.name()));
    patch.insert("metadata".into(), Value::Object(metadata));

    if let Some(spec) = &input.spec {
        let Some(fields) = spec.as_object() else {
            return Err(ToolError::new(
                codes::INVALID_INPUT,
                Remedy::RetryAfterChange,
                "`spec` must be an object of the fields to change. To remove one field, set it to \
                 `null` inside `spec`.",
            )
            .with_details(json!({ "field": "spec" })));
        };

        if fields.contains_key(CUSTOM_FIELDS_KEY) {
            return Err(custom_fields_refused("spec.customFields"));
        }

        patch.insert("spec".into(), spec.clone());
    }

    Ok(Value::Object(patch))
}

/// The metadata fields the caller mentioned, `null`s kept as deletions.
fn metadata_patch(metadata: &ItemMetadataPatch) -> Map<String, Value> {
    [
        ("title", &metadata.title),
        ("description", &metadata.description),
        ("labels", &metadata.labels),
        ("tags", &metadata.tags),
        ("annotations", &metadata.annotations),
        ("links", &metadata.links),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.clone().map(|value| (key.to_string(), value)))
    .collect()
}

/// Custom fields are refused rather than silently ignored — a model told its write
/// succeeded must not later read the old value.
fn custom_fields_refused(field: &str) -> ToolError {
    ToolError::new(
        codes::INVALID_INPUT,
        Remedy::RetryAfterChange,
        "Custom fields cannot be set with apply_item: a catalog write ignores them. Remove \
         `customFields` and send the rest.",
    )
    .with_details(json!({ "field": field }))
}
