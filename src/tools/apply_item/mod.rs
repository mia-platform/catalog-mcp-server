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
    ConflictPolicy, ItemAddress, Remedy, ResourceVersionIn, ToolError, TypeCoordinates, WriteCycle,
    error::codes, is_valid_kind, is_valid_name, resolve_kind_or_suggest,
};
use regex::Regex;
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;

/// The tool's input → merge-patch document, and the `customFields` check.
mod patch;

/// The `null`-keeping deserializer, shared with `apply_item_type`.
pub(crate) use patch::present;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "apply_item";

/// What the tool does. Every sentence is load-bearing: preservation, deletion, array semantics,
/// and custom fields. The last names no tool, because `patch_item_custom_fields` is not built
/// yet and a pointer to a tool that does not exist would send the model looking.
const TOOL_DESCRIPTION: &str = "Creates or updates a catalog item. Send only the fields to \
     change; anything left out is kept. `null` removes a field. Lists such as `tags` are \
     replaced, not appended. Custom fields cannot be set here. Returns `created`, the `changed` \
     field paths (empty if nothing changed) and `retried` (a conflict was resolved by \
     re-reading). Address the item by `name` and `kind` (and `group` for a shared kind); put the \
     changes in `spec` and `metadata`.";

/// The longest `name`, in bytes.
pub const MAX_NAME_BYTES: usize = 256;

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// How many offending paths a schema violation's next step names — `get_item_schema`'s own
/// `fields` bound.
const MAX_HINTED_FIELDS: usize = 20;

/// One location in the engine's schema-violation message, which reads
/// `Body does not conform to Item Type Definition schema: path "/spec/x": <reason>; path …`
/// (`models/lib/json_schema.rs`). Each location is a JSON Pointer into the item.
static VIOLATION_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r#"path "([^"]*)": "#).expect("VIOLATION_PATH_RE is a constant pattern")
});

/// Arguments for `apply_item`; `group` says which type a shared `kind` means.
///
/// `owner` and `followers` are absent **by construction**, `resourceVersion` because the
/// server reads it itself, and `customFields` because a `PUT` ignores it. Unknown
/// arguments — any of those three included — are refused by name.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct ApplyItemInput {
    /// The item's name.
    #[serde(rename = "name")]
    pub name: String,

    /// The item's type.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// Fields to set in the item's spec.
    #[serde(rename = "spec", default, deserialize_with = "patch::present")]
    pub spec: Option<Value>,

    /// Metadata fields to set.
    #[serde(rename = "metadata")]
    pub metadata: Option<ItemMetadataPatch>,
}

/// The mutable metadata an agent may set. Each field is a raw value so that `null` reaches the
/// merge as a deletion rather than collapsing into "not mentioned".
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
#[schemars(transform = patch::without_description)]
pub struct ItemMetadataPatch {
    /// A human-readable title.
    #[serde(rename = "title", default, deserialize_with = "patch::present")]
    pub(crate) title: Option<Value>,

    /// A short description.
    #[serde(rename = "description", default, deserialize_with = "patch::present")]
    pub(crate) description: Option<Value>,

    /// Searchable key/value labels.
    #[serde(rename = "labels", default, deserialize_with = "patch::present")]
    pub(crate) labels: Option<Value>,

    /// Free-form tags.
    #[serde(rename = "tags", default, deserialize_with = "patch::present")]
    pub(crate) tags: Option<Value>,

    /// Key/value annotations, not searchable.
    #[serde(rename = "annotations", default, deserialize_with = "patch::present")]
    pub(crate) annotations: Option<Value>,

    /// Links, each `{url, title?}`.
    #[serde(rename = "links", default, deserialize_with = "patch::present")]
    pub(crate) links: Option<Value>,
}

/// What the write did — not the item, which the model already knows.
#[derive(Serialize)]
struct Applied {
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

    /// `true` when the item did not exist.
    #[serde(rename = "created")]
    created: bool,

    /// The field paths that differ. **Present and empty** on a no-op.
    #[serde(rename = "changed")]
    changed: Vec<String>,

    /// `true` when a `409` was resolved by re-reading.
    #[serde(rename = "retried")]
    retried: bool,
}

/// `apply_item` — create or merge-patch one item.
///
/// **The read-merge-write cycle is data-loss protection**: a `PUT` replaces every mutable
/// column, so a body built from the patch alone would wipe everything it did not mention. The
/// cycle, the merge and the conflict rule are `catalog-client`'s; this tool supplies the patch.
pub struct ApplyItem;

impl Tool for ApplyItem {
    type Input = ApplyItemInput;

    /// Not destructive, idempotent: applying the same patch twice reaches the same
    /// state. `readOnlyHint: false` is the default, so it is not emitted.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<ApplyItemInput>(
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

        // Never guess the type of a write: an unknown kind comes back with near matches, a shared
        // one with its candidates.
        let coordinates =
            resolve_kind_or_suggest(context.engine(), &input.kind, input.group.as_deref()).await?;

        let address = ItemAddress::new(
            &coordinates.group,
            &coordinates.version,
            &coordinates.family,
            &input.name,
        )?;
        let document = patch::document(&input, &address)?;

        let outcome = WriteCycle::new(
            context.engine(),
            ConflictPolicy::RetryOnce,
            ResourceVersionIn::Body,
        )
        .apply(&address, &document)
        .await
        .map_err(|error| with_schema_hint(error, &coordinates))?;

        let applied = Applied {
            name: input.name,
            kind: coordinates.kind,
            group: coordinates.group,
            version: coordinates.version,
            family: coordinates.family,
            created: outcome.created,
            changed: outcome.changed,
            retried: outcome.retried,
        };

        serde_json::to_value(applied)
            .map(ToolOutput::new)
            .map_err(|err| {
                ToolError::new(
                    codes::SERVER_DEFECT,
                    Remedy::Escalate,
                    format!("The write's outcome could not be rendered: {err}"),
                )
            })
    }
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &ApplyItemInput) -> Result<(), ToolError> {
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

    validate_group(input.group.as_deref(), true)
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// The tool's most valuable error, made actionable: a schema violation carries the offending field
/// in `details.path`, and a next step that fetches **just** the rules that failed.
///
/// Only an `invalid_input` whose message names locations is touched; any other error passes
/// through unchanged, so nothing is claimed about a rejection this server cannot read.
fn with_schema_hint(error: ToolError, coordinates: &TypeCoordinates) -> ToolError {
    if error.code != codes::INVALID_INPUT {
        return error;
    }

    let paths = violation_paths(&error.message);
    let Some(first) = paths.first() else {
        return error;
    };

    let mut details = error
        .details
        .as_deref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    details.insert("path".to_string(), json!(first));

    let mut fields: Vec<String> = Vec::new();
    for path in &paths {
        let field = schema_field(path);
        if !field.is_empty() && !fields.contains(&field) {
            fields.push(field);
        }
    }
    fields.truncate(MAX_HINTED_FIELDS);

    let next_step = format!(
        "call get_item_schema with {} for the rules those fields follow, then retry",
        json!({ "kind": coordinates.kind, "group": coordinates.group, "fields": fields })
    );

    error
        .with_details(Value::Object(details))
        .with_next_step(next_step)
}

/// Every location the engine's message names, as dotted paths (`spec.steps.0.title`), in order.
/// The document root (`/`) is not a field and is left out.
fn violation_paths(message: &str) -> Vec<String> {
    VIOLATION_PATH_RE
        .captures_iter(message)
        .filter_map(|captures| captures.get(1))
        .map(|pointer| dotted(pointer.as_str()))
        .filter(|path| !path.is_empty())
        .collect()
}

/// A JSON Pointer as a dotted path, its escapes (`~1`, `~0`) undone.
fn dotted(pointer: &str) -> String {
    pointer
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".")
}

/// A dotted item path as `get_item_schema`'s `fields` takes it: array indices dropped, since its
/// walk steps into an array's `items` by itself.
fn schema_field(path: &str) -> String {
    path.split('.')
        .filter(|segment| !segment.bytes().all(|byte| byte.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests;
