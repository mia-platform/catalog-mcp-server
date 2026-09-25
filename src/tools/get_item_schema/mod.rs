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
use crate::{
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::arguments::validate_group,
};
use catalog_client::{
    Remedy, ToolError, coordinates_of,
    error::codes,
    find_item_type_or_suggest, is_valid_kind,
    models::{ItemTypeDefinition, TypeVersion},
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "get_item_schema";

/// What the tool does (T6 §3). Its second sentence is the hand-off from `list_catalog_types`,
/// stated in the one place the model can act on it.
const TOOL_DESCRIPTION: &str = "Returns the schema of one catalog type: the fields an item of \
     that kind can have, with their types and descriptions. Call this before creating or updating an \
     item, or when you need more detail about a type than `list_catalog_types` gave you.";

/// The longest `kind`, in bytes (T6 §3).
pub const MAX_KIND_BYTES: usize = 128;

/// The longest `version`, in bytes (T6 §3).
pub const MAX_VERSION_BYTES: usize = 64;

/// The key a version's JSON Schema sits under inside `spec.versions[].schema`.
const OPENAPI_SCHEMA_KEY: &str = "openAPIV31Schema";

/// The key of a selectable field's path inside `spec.versions[].selectableFields[]`.
const JSON_PATH_KEY: &str = "jsonPath";

/// Arguments for `get_item_schema` (T6 §3).
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct GetItemSchemaInput {
    /// The type's kind, e.g. "Service". Not the item-type-definition name.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's API group. Only needed when several types share the kind.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// A specific version. Absent means the served one.
    #[serde(rename = "version")]
    pub version: Option<String>,
}

/// The response (T6 §4), in its serialised order.
///
/// Dropped on purpose: `uid`, `urn`, the timestamps, `resourceVersion`, `spec.scope` (always
/// `Tenant`) and the selected version's `served`/`deprecated` flags — none is actionable, and the
/// coordinates an agent needs are the three it addresses items by.
#[derive(Serialize)]
struct ItemSchemaOutput {
    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "family")]
    family: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "version")]
    version: String,

    /// `spec.names.displaySingular` — **never** `metadata.title`, which no shipped type sets
    /// (T6-D7).
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,

    /// `spec.names.displayPlural`.
    #[serde(rename = "displayPlural", skip_serializing_if = "Option::is_none")]
    display_plural: Option<String>,

    /// `metadata.description`.
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    /// `spec.llmDescription`, **in full** and unconditionally (T6-D3); omitted when absent or
    /// blank, and never synthesised from `description`.
    #[serde(rename = "llmDescription", skip_serializing_if = "Option::is_none")]
    llm_description: Option<String>,

    /// The selected version's JSON Schema, **whole, always** (§5, D34): a truncated schema is
    /// not a smaller schema, it is a wrong one.
    #[serde(rename = "schema", skip_serializing_if = "Option::is_none")]
    schema: Option<Value>,

    /// The selected version's `selectableFields[].jsonPath`, flattened to strings — what T2's
    /// `fields` validates against. Omitted when the type has none, which is most of them.
    #[serde(rename = "selectableFields", skip_serializing_if = "Vec::is_empty")]
    selectable_fields: Vec<String>,

    #[serde(rename = "historyEnabled")]
    history_enabled: bool,
}

/// T6 · `get_item_schema` — one type's schema, whole, and what an agent needs to write against it.
///
/// **One** engine call — the core's `kind` lookup, the same request coordinate resolution makes
/// (T6-D1) — and a projection. Nothing is capped, depth-limited or elided (D34).
pub struct GetItemSchema;

impl Tool for GetItemSchema {
    type Input = GetItemSchemaInput;

    /// `readOnlyHint: true`; every other hint is the specification's default (D16).
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<GetItemSchemaInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().read_only(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;

        // An unknown kind comes back with near matches (T2-D9), a shared one with its candidates
        // (DR-80), and two rows for one `(group, kind)` as a `server_defect` (T6-D2) — all the
        // core's.
        let definition =
            find_item_type_or_suggest(context.engine(), &input.kind, input.group.as_deref())
                .await?;

        // The core's rule, and its `unaddressable_type` when nothing is served: a schema nothing
        // can address items against would be worse than an error (D30).
        let coordinates = coordinates_of(&definition, &input.kind)?;
        let version = select_version(&definition, &coordinates.version, input.version.as_deref())?;

        let output = project(&definition, version, coordinates.family, coordinates.group);
        let payload = serde_json::to_value(&output).map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The schema could not be rendered: {err}"),
            )
        })?;

        Ok(ToolOutput::new(payload))
    }
}

/// NFR-10 — T6 §3's bounds, checked before anything reaches the engine.
fn validate(input: &GetItemSchemaInput) -> Result<(), ToolError> {
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

    if let Some(version) = &input.version
        && (version.is_empty() || version.len() > MAX_VERSION_BYTES)
    {
        return Err(invalid(
            "version",
            format!("`version` must be between 1 and {MAX_VERSION_BYTES} bytes."),
        ));
    }

    validate_group(input.group.as_deref(), true)
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// The version whose schema is returned (T6-D8).
///
/// Absent: the one the core's rule selected. Given: it must exist **and** be served, or it is an
/// error naming the versions that are — **never** silently substituted, which would return a schema
/// the model cannot address items against.
fn select_version<'a>(
    definition: &'a ItemTypeDefinition,
    selected: &str,
    requested: Option<&str>,
) -> Result<&'a TypeVersion, ToolError> {
    let wanted = requested.unwrap_or(selected);

    definition
        .spec
        .versions
        .iter()
        .find(|version| version.name == wanted && version.served)
        .ok_or_else(|| {
            let served: Vec<&str> = definition
                .spec
                .versions
                .iter()
                .filter(|version| version.served)
                .map(|version| version.name.as_str())
                .collect();

            invalid(
                "version",
                format!(
                    "`{wanted}` is not a served version of `{}`.",
                    definition.spec.names.kind
                ),
            )
            .with_details(json!({ "field": "version", "servedVersions": served }))
        })
}

/// Projects the definition and its selected version into the response (T6 §4).
fn project(
    definition: &ItemTypeDefinition,
    version: &TypeVersion,
    family: String,
    group: String,
) -> ItemSchemaOutput {
    let spec = &definition.spec;

    ItemSchemaOutput {
        kind: spec.names.kind.clone(),
        family,
        group,
        version: version.name.clone(),
        display_name: spec.names.display_singular.clone(),
        display_plural: spec.names.display_plural.clone(),
        description: definition.metadata.description.clone(),
        llm_description: spec
            .llm_description
            .clone()
            .filter(|description| !description.trim().is_empty()),
        // The JSON Schema proper; a schema in any other shape is returned as it came rather than
        // dropped, because losing it is the one failure this tool exists to prevent.
        schema: version.schema.as_ref().map(|schema| {
            schema
                .get(OPENAPI_SCHEMA_KEY)
                .cloned()
                .unwrap_or_else(|| schema.clone())
        }),
        selectable_fields: version
            .selectable_fields
            .iter()
            .filter_map(|field| field.get(JSON_PATH_KEY).and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        history_enabled: spec.history_enabled(),
    }
}

#[cfg(test)]
mod tests;
