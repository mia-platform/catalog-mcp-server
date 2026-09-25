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
    ItemTypeDocument, Remedy, ToolError, coordinates_of, error::codes,
    find_item_type_document_or_suggest, is_valid_kind, models::ItemTypeDefinition,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// T6's `fields` mode (DR-86): the schema of just the fields an agent is about to change.
mod fields;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "get_item_schema";

/// What the tool does (DR-86). Full by default — creating an item and editing the type both need
/// the whole thing — and `fields` for a change to an existing item.
const TOOL_DESCRIPTION: &str = "Returns one catalog type's definition, including the schema its \
     items follow. Call it before creating an item or editing the type. To change an existing \
     item, pass `fields` to get only those fields' schema.";

/// The longest `kind`, in bytes (T6 §3).
pub const MAX_KIND_BYTES: usize = 128;

/// The longest `version`, in bytes (T6 §3).
pub const MAX_VERSION_BYTES: usize = 64;

/// The most `fields` one call may ask for.
pub const MAX_FIELDS: usize = 20;

/// The longest field path, in bytes.
pub const MAX_FIELD_PATH_BYTES: usize = 256;

/// The key a version's JSON Schema sits under inside `spec.versions[].schema`.
const OPENAPI_SCHEMA_KEY: &str = "openAPIV31Schema";

/// `metadata` keys that are routing data, not the type: never actionable, and `name` is returned
/// at the top level.
const METADATA_NOISE: [&str; 6] = [
    "name",
    "uid",
    "urn",
    "family",
    "creationTimestamp",
    "updateTimestamp",
];

/// Arguments for `get_item_schema` (T6 §3, DR-80, DR-86).
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct GetItemSchemaInput {
    /// The type's kind, e.g. "Service".
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's API group. Only needed when several types share the kind.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// A specific version. Absent means the served one.
    #[serde(rename = "version")]
    pub version: Option<String>,

    /// Only these fields' schema, e.g. ["spec.lifecycle"]. Absent returns the whole definition.
    #[serde(rename = "fields")]
    pub fields: Option<Vec<String>>,
}

/// The default answer: the **whole** definition (DR-86).
///
/// `spec` is the engine's, untouched — every version with its flags, selectable fields in their real
/// shape, history and audit settings — because a model that edits the type with T12 rebuilds
/// arrays like `versions` from this, and anything missing here would be erased by that edit.
#[derive(Serialize)]
struct FullDefinition {
    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "family")]
    family: String,

    /// The version items are addressed under — the served one, or the one asked for.
    #[serde(rename = "version")]
    version: String,

    /// The definition's own name, `<family>.<group>` — FR-10's key.
    #[serde(rename = "name")]
    name: String,

    /// What remains of `metadata` once routing data is removed; omitted when nothing does.
    #[serde(rename = "metadata", skip_serializing_if = "Option::is_none")]
    metadata: Option<Value>,

    #[serde(rename = "spec")]
    spec: Value,
}

/// The `fields` answer: the schema of each requested field of the selected version, and the
/// definitions they reference (DR-86).
#[derive(Serialize)]
struct FieldsAnswer {
    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "family")]
    family: String,

    #[serde(rename = "version")]
    version: String,

    #[serde(rename = "fields")]
    fields: Map<String, Value>,

    /// Named `$defs` so that every `#/$defs/<name>` inside `fields` resolves against this answer.
    #[serde(rename = "$defs", skip_serializing_if = "Map::is_empty")]
    defs: Map<String, Value>,
}

/// T6 · `get_item_schema` — one type's definition, whole, or the schema of chosen fields.
///
/// **One** engine call — the core's `kind` lookup (T6-D1) — and no reshaping of what the engine
/// returned beyond removing routing data. Nothing is capped, depth-limited or elided (D34); a
/// `fields` subset is asked for explicitly and is never a quieter version of the whole.
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
        let document = find_item_type_document_or_suggest(
            context.engine(),
            &input.kind,
            input.group.as_deref(),
        )
        .await?;

        // The core's rule, and its `unaddressable_type` when nothing is served (D30).
        let coordinates = coordinates_of(&document.definition, &input.kind)?;
        let version = select_version(
            &document.definition,
            &coordinates.version,
            input.version.as_deref(),
        )?;

        let payload = match &input.fields {
            Some(paths) => {
                let root = version_schema(&document.raw, &version).ok_or_else(|| {
                    ToolError::new(
                        codes::UNADDRESSABLE_TYPE,
                        Remedy::Escalate,
                        format!(
                            "Version `{version}` of `{}` declares no schema to read fields from.",
                            input.kind
                        ),
                    )
                })?;
                let extracted = fields::extract(root, paths)?;

                to_payload(&FieldsAnswer {
                    kind: document.definition.spec.names.kind.clone(),
                    group: coordinates.group,
                    family: coordinates.family,
                    version,
                    fields: extracted.fields,
                    defs: extracted.defs,
                })?
            }
            None => to_payload(&full_definition(
                document,
                version,
                coordinates.family,
                coordinates.group,
            ))?,
        };

        Ok(ToolOutput::new(payload))
    }
}

/// NFR-10 — T6 §3's bounds and `fields`', checked before anything reaches the engine.
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

    if let Some(paths) = &input.fields {
        if paths.is_empty() || paths.len() > MAX_FIELDS {
            return Err(invalid(
                "fields",
                format!(
                    "`fields` takes between 1 and {MAX_FIELDS} paths; omit it for the whole \
                     definition."
                ),
            ));
        }

        if let Some(path) = paths.iter().find(|path| !is_field_path(path)) {
            return Err(invalid(
                "fields",
                format!(
                    "`{path}` is not a field path: dotted names such as `spec.lifecycle`, at most \
                     {MAX_FIELD_PATH_BYTES} bytes."
                ),
            ));
        }
    }

    validate_group(input.group.as_deref(), true)
}

/// Whether `path` is a dotted field path with no empty segment.
fn is_field_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_FIELD_PATH_BYTES
        && path.split('.').all(|segment| !segment.is_empty())
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// The version to report and to read `fields` from (T6-D8).
///
/// Absent: the one the core's rule selected. Given: it must exist **and** be served, or it is an
/// error naming the versions that are — **never** silently substituted.
fn select_version(
    definition: &ItemTypeDefinition,
    selected: &str,
    requested: Option<&str>,
) -> Result<String, ToolError> {
    let wanted = requested.unwrap_or(selected);

    if definition
        .spec
        .versions
        .iter()
        .any(|version| version.name == wanted && version.served)
    {
        return Ok(wanted.to_string());
    }

    let served: Vec<&str> = definition
        .spec
        .versions
        .iter()
        .filter(|version| version.served)
        .map(|version| version.name.as_str())
        .collect();

    Err(invalid(
        "version",
        format!(
            "`{wanted}` is not a served version of `{}`.",
            definition.spec.names.kind
        ),
    )
    .with_details(json!({ "field": "version", "servedVersions": served })))
}

/// The raw `openAPIV31Schema` of the named version, as the engine sent it.
fn version_schema<'a>(raw: &'a Value, version: &str) -> Option<&'a Value> {
    raw.get("spec")?
        .get("versions")?
        .as_array()?
        .iter()
        .find(|candidate| candidate.get("name").and_then(Value::as_str) == Some(version))?
        .get("schema")?
        .get(OPENAPI_SCHEMA_KEY)
}

/// The whole definition, minus routing data (DR-86).
fn full_definition(
    document: ItemTypeDocument,
    version: String,
    family: String,
    group: String,
) -> FullDefinition {
    let mut raw = document.raw;

    let spec = raw.get_mut("spec").map(Value::take).unwrap_or(Value::Null);

    let metadata = raw
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
        .map(|metadata| {
            for key in METADATA_NOISE {
                metadata.remove(key);
            }
            std::mem::take(metadata)
        })
        .filter(|metadata| !metadata.is_empty())
        .map(Value::Object);

    // Only `metadata` and `spec` are carried over. The document's own `apiVersion` and `kind`
    // describe the *definition* resource, not the type, and `resourceVersion` is T12's to read for
    // itself (T12-D8) — so all three are dropped by construction.
    FullDefinition {
        kind: document.definition.spec.names.kind.clone(),
        group,
        family,
        version,
        name: document.definition.metadata.name,
        metadata,
        spec,
    }
}

/// Serialises an answer, reporting the impossible failure as ours.
fn to_payload<T: Serialize>(answer: &T) -> Result<Value, ToolError> {
    serde_json::to_value(answer).map_err(|err| {
        ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            format!("The type definition could not be rendered: {err}"),
        )
    })
}

#[cfg(test)]
mod tests;
