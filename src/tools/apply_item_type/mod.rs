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
    tools::{apply_item::present, arguments::validate_group},
};
use catalog_client::{
    ConflictPolicy, EngineClient, Existence, FieldPath, ItemTypeAddress, ItemTypeDocument,
    Predicate, QueryValue, Remedy, ResourceVersionIn, ToolError, WriteCycle, WriteOutcome,
    error::codes, find_item_type_document_if_any, find_item_type_document_or_suggest,
    is_valid_kind, ops::ListQuery,
};
use regex::Regex;
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

/// `ignored`, `schemaChanged`, `versionsRemoved` and `backgroundJobs`.
mod report;

use report::{IDENTITY_REASON, Ignored};

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "apply_item_type";

/// What the tool does. `spec.versions` is replaced whole, so it names `get_item_schema`'s `spec`
/// as the place to start from.
///
/// The last sentence matches `delete_item`'s: behind the gateway the count is filtered to what the
/// caller may read, so it is stated as that and never as the total.
const TOOL_DESCRIPTION: &str = "Creates or updates a catalog type definition. Send only what you \
     want to change; `spec.versions` is replaced whole, so start from `get_item_schema`'s `spec`. \
     Some fields cannot be changed after creation — the type's `kind`, `plural`, `group`, and its \
     history and audit settings — and attempts to change them are reported back as ignored. \
     Changing a type's schema does not re-validate items that already exist. `existingItems` \
     counts the items you can read.";

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// The kinds whose items the global count does not include (the engine's "core" families), so
/// `existingItems` cannot be counted for them that way.
const UNCOUNTED_CORE_KINDS: [&str; 4] = [
    "Relationship",
    "RelationshipType",
    "RelationshipConstraint",
    "CustomField",
];

/// The group the core kinds live in.
const CORE_GROUP: &str = ItemTypeAddress::API_GROUP;

/// One location in the engine's definition-validation message, e.g.
/// `'spec.versions[0].schema.openAPIV31Schema' is not valid: …` (`ITDSpec::validate`).
static VERSION_LOCATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r"'(spec\.versions\[\d+\](?:\.[A-Za-z0-9]+)*)'")
        .expect("VERSION_LOCATION_RE is a constant pattern")
});

/// A JSON Pointer inside the schema that failed, `path "/properties/x": …`.
static SCHEMA_POINTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r#"path "([^"]*)": "#).expect("SCHEMA_POINTER_RE is a constant pattern")
});

/// Arguments for `apply_item_type`; `group` says which type a shared `kind` means.
///
/// `spec` and `metadata` are raw JSON: `openAPIV31Schema` is an arbitrary JSON Schema that cannot be
/// usefully typed here, and the read-only set is the engine's to enforce and report.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct ApplyItemTypeInput {
    /// The type's kind, e.g. "Service".
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    // Undocumented in the schema, as in `apply_item`: the description states the merge rules, and
    // a description here is paid for in every `tools/list`.
    #[serde(rename = "spec", default, deserialize_with = "present")]
    pub spec: Option<Value>,

    #[serde(rename = "metadata", default, deserialize_with = "present")]
    pub metadata: Option<Value>,
}

/// What the write did. `warnings`, verbatim, is added by the runtime.
#[derive(Serialize)]
struct Applied {
    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    /// The definition's name, `<plural>.<group>`.
    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "created")]
    created: bool,

    #[serde(rename = "changed")]
    changed: Vec<String>,

    /// Present and empty when nothing was ignored.
    #[serde(rename = "ignored")]
    ignored: Vec<Ignored>,

    #[serde(rename = "schemaChanged")]
    schema_changed: bool,

    /// Only when the schema changed or a served version went away; absent when it could not be
    /// counted, which a warning then says. Behind the gateway, the items the caller can read.
    #[serde(rename = "existingItems", skip_serializing_if = "Option::is_none")]
    existing_items: Option<u64>,

    /// Only when a version served before the write is no longer served.
    #[serde(rename = "versionsRemoved", skip_serializing_if = "Vec::is_empty")]
    versions_removed: Vec<String>,

    /// Present and empty when nothing was started.
    #[serde(rename = "backgroundJobs")]
    background_jobs: Vec<&'static str>,
}

/// `apply_item_type` — create or merge-patch one type definition, and report what the engine did
/// not apply and what the change means for the items already stored.
///
/// The same read-merge-write cycle as `apply_item` — `PUT` replaces here too — with **`Report`** on
/// a `409` and the definition read and written raw.
pub struct ApplyItemType;

impl Tool for ApplyItemType {
    type Input = ApplyItemTypeInput;

    /// Destructive (the default, not emitted) and idempotent.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<ApplyItemTypeInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().idempotent(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;
        let engine = context.engine();

        let existing =
            find_item_type_document_if_any(engine, &input.kind, input.group.as_deref()).await?;

        let plan = match existing {
            Some(document) => update(&input, &document)?,
            None => create(engine, &input).await?,
        };

        let outcome = WriteCycle::new(engine, ConflictPolicy::Report, ResourceVersionIn::Body)
            .apply_item_type(&plan.address, &plan.patch, plan.existence)
            .await
            .map_err(|error| write_error(error, &plan.address))?;

        let (applied, count_warning) = report(engine, &input.kind, &plan, outcome).await;

        let output = serde_json::to_value(applied)
            .map(ToolOutput::new)
            .map_err(|err| {
                ToolError::new(
                    codes::SERVER_DEFECT,
                    Remedy::Escalate,
                    format!("The write happened, but its report could not be rendered: {err}"),
                )
            })?;

        Ok(match count_warning {
            Some(warning) => output.with_warning(warning),
            None => output,
        })
    }
}

/// What will be written, where, and what must be found there first.
struct WritePlan {
    address: ItemTypeAddress,
    patch: Value,
    existence: Existence,

    /// Fields this tool held back from the patch rather than send, with their reason.
    held_back: Vec<Ignored>,
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &ApplyItemTypeInput) -> Result<(), ToolError> {
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
        ));
    }

    for (field, value) in [("spec", &input.spec), ("metadata", &input.metadata)] {
        if value.as_ref().is_some_and(|value| !value.is_object()) {
            return Err(invalid(
                field,
                format!(
                    "`{field}` must be an object of the fields to change. To remove one field, \
                     set it to `null` inside `{field}`."
                ),
            ));
        }
    }

    validate_group(input.group.as_deref(), true)
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// A string at `pointer` in `value`, if there is one.
fn string_at<'a>(value: Option<&'a Value>, pointer: &str) -> Option<&'a str> {
    value?.pointer(pointer)?.as_str()
}

/// The update path: the definition the lookup found, patched with what the caller sent.
///
/// `spec.group`, `spec.names.plural` and `metadata.name` are the definition's **address**: the
/// engine requires `metadata.name` to equal `<plural>.<group>`, so a patch changing any of them is
/// refused with a `400` about the name rather than reported as ignored. They are held back here
/// and reported in `ignored`, as the tool promises, and everything else is sent.
fn update(input: &ApplyItemTypeInput, document: &ItemTypeDocument) -> Result<WritePlan, ToolError> {
    let spec = &document.definition.spec;
    let address = ItemTypeAddress::new(&spec.group, &spec.names.plural)?;

    let mut patch = Map::new();
    let mut held_back = Vec::new();

    if let Some(Value::Object(fields)) = &input.spec {
        let mut fields = fields.clone();

        if fields
            .get("group")
            .is_some_and(|group| group != &json!(spec.group))
        {
            fields.remove("group");
            held_back.push(identity("spec.group"));
        }

        if let Some(Value::Object(names)) = fields.get_mut("names")
            && names
                .get("plural")
                .is_some_and(|plural| plural != &json!(spec.names.plural))
        {
            names.remove("plural");
            held_back.push(identity("spec.names.plural"));
        }

        patch.insert("spec".into(), Value::Object(fields));
    }

    if let Some(Value::Object(fields)) = &input.metadata {
        let mut fields = fields.clone();

        if fields
            .get("name")
            .is_some_and(|name| name != &json!(address.name()))
        {
            fields.remove("name");
            held_back.push(identity("metadata.name"));
        }

        patch.insert("metadata".into(), Value::Object(fields));
    }

    Ok(WritePlan {
        address,
        patch: Value::Object(patch),
        existence: Existence::Present,
        held_back,
    })
}

/// A field held back because it names the type.
fn identity(field: &str) -> Ignored {
    Ignored {
        field: field.to_string(),
        reason: IDENTITY_REASON,
    }
}

/// The create path: no type has this kind (in `group`, when given).
///
/// A create names the new type's address — `group` and `spec.names.plural` — and at least one
/// version, since a type with none is unaddressable on arrival. A call carrying **none** of
/// the three is not an attempted create but, most likely, an update of a misspelled kind: it gets
/// the lookup's `not_found` with near matches, and a next step for either reading.
async fn create(engine: &EngineClient, input: &ApplyItemTypeInput) -> Result<WritePlan, ToolError> {
    let spec = input.spec.as_ref();
    let spec_group = string_at(spec, "/group");
    let plural = string_at(spec, "/names/plural");
    let versions = spec
        .and_then(|spec| spec.get("versions"))
        .and_then(Value::as_array);

    if spec_group.is_none() && plural.is_none() && versions.is_none() {
        return Err(no_such_kind(engine, input).await);
    }

    let group = match (input.group.as_deref(), spec_group) {
        (Some(argument), Some(field)) if argument != field => {
            return Err(invalid(
                "group",
                format!("`group` is `{argument}` but `spec.group` is `{field}`; give one of them."),
            ));
        }
        (Some(group), _) | (None, Some(group)) => group,
        (None, None) => return Err(missing("spec.group")),
    };
    let plural = plural.ok_or_else(|| missing("spec.names.plural"))?;
    if versions.is_none_or(Vec::is_empty) {
        return Err(missing("spec.versions"));
    }

    if let Some(named) = string_at(spec, "/names/kind")
        && named != input.kind
    {
        return Err(invalid(
            "spec.names.kind",
            format!(
                "`kind` is `{}` but `spec.names.kind` is `{named}`; give one of them.",
                input.kind
            ),
        ));
    }

    let address = ItemTypeAddress::new(group, plural)?;

    let mut spec = spec.cloned().unwrap_or_else(|| json!({}));
    spec["group"] = json!(group);
    spec["names"]["kind"] = json!(input.kind);

    let mut metadata = input.metadata.clone().unwrap_or_else(|| json!({}));
    metadata["name"] = json!(address.name());

    Ok(WritePlan {
        patch: json!({
            "apiVersion": ItemTypeAddress::api_version(),
            "kind": ItemTypeAddress::KIND,
            "metadata": metadata,
            "spec": spec,
        }),
        address,
        existence: Existence::Absent,
        held_back: Vec::new(),
    })
}

/// What a create needs and was not given.
fn missing(field: &str) -> ToolError {
    invalid(
        field,
        format!(
            "Creating a type needs `spec.group` (or `group`), `spec.names.plural` and a \
             non-empty `spec.versions`; `{field}` is missing."
        ),
    )
}

/// An unknown kind with nothing that says "create": the core's `not_found`, near matches included.
async fn no_such_kind(engine: &EngineClient, input: &ApplyItemTypeInput) -> ToolError {
    let error =
        match find_item_type_document_or_suggest(engine, &input.kind, input.group.as_deref()).await
        {
            Err(error) => error,
            // It appeared between the two lookups: the next call will find and update it.
            Ok(_) => ToolError::new(
                codes::CONFLICT,
                Remedy::Retry,
                format!("A `{}` type was created while this call ran.", input.kind),
            ),
        };

    error.with_next_step(
        "check the kind with list_catalog_types to update a type; to create one, send \
         `spec.group`, `spec.names.plural` and `spec.versions`",
    )
}

/// The write's failures, in this tool's words.
fn write_error(error: ToolError, address: &ItemTypeAddress) -> ToolError {
    match error.code {
        // `Existence::Absent` found the name taken: another type — another kind — owns it.
        codes::CONFLICT if error.remedy == Remedy::RetryAfterChange => ToolError::new(
            codes::CONFLICT,
            Remedy::RetryAfterChange,
            format!(
                "A type named `{}` already exists with another kind; choose another \
                 `spec.names.plural`, or update that type.",
                address.name()
            ),
        )
        .with_details(json!({ "name": address.name() })),
        // A type is rarely written concurrently, so this is someone reshaping it.
        codes::CONFLICT => ToolError::new(
            codes::CONFLICT,
            Remedy::RetryLater,
            "Another writer changed this type while it was being written; it was not changed.",
        )
        .with_next_step("call get_item_schema to read it again before retrying"),
        codes::INVALID_INPUT => with_location(error),
        _ => error,
    }
}

/// A definition the engine rejected, with where: `details.path` names the version, and
/// `details.schemaPath` the pointer inside its schema when the engine gives one.
fn with_location(error: ToolError) -> ToolError {
    let Some(location) = VERSION_LOCATION_RE
        .captures(&error.message)
        .and_then(|captures| captures.get(1))
    else {
        return error;
    };

    let mut details = error
        .details
        .as_deref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    details.insert("path".into(), json!(location.as_str()));

    if let Some(pointer) = SCHEMA_POINTER_RE
        .captures(&error.message)
        .and_then(|captures| captures.get(1))
    {
        details.insert("schemaPath".into(), json!(pointer.as_str()));
    }

    error.with_details(Value::Object(details))
}

/// The answer, from the cycle's outcome — and, when the schema or the served set moved, one count
/// of the items affected, which never blocks or undoes the write.
async fn report(
    engine: &EngineClient,
    kind: &str,
    plan: &WritePlan,
    outcome: WriteOutcome,
) -> (Applied, Option<String>) {
    let empty = json!({});
    let before = outcome.before.as_ref().unwrap_or(&empty);

    let (schema_changed, versions_removed) = if outcome.created {
        (false, Vec::new())
    } else {
        (
            report::schema_changed(before, &outcome.after),
            report::versions_removed(before, &outcome.after),
        )
    };

    let (existing_items, count_warning) = if schema_changed || !versions_removed.is_empty() {
        let mut versions = report::version_names(before);
        for version in report::version_names(&outcome.after) {
            if !versions.contains(&version) {
                versions.push(version);
            }
        }

        match count_items(engine, plan.address.group(), kind, &versions).await {
            Ok(count) => (Some(count), None),
            Err(reason) => (None, Some(reason)),
        }
    } else {
        (None, None)
    };

    let applied = Applied {
        kind: kind.to_string(),
        group: plan.address.group().to_string(),
        name: plan.address.name().to_string(),
        created: outcome.created,
        changed: outcome.changed,
        ignored: report::ignored(plan.held_back.clone(), &outcome.warnings),
        schema_changed,
        existing_items,
        versions_removed,
        background_jobs: report::background_jobs(outcome.created, &outcome.after),
    };

    (applied, count_warning)
}

/// How many items the type has, **across every version** it declared before or after the write —
/// so items under a version just removed are counted too.
///
/// One global count, filtered on `kind` and each `apiVersion`: the family count would cover one
/// version only, and cannot reach a version no longer served. The core kinds are not in the global
/// count at all, so for them the count is reported as unavailable rather than as a false `0`.
///
/// # Errors
///
/// The warning to add when the count is unavailable.
async fn count_items(
    engine: &EngineClient,
    group: &str,
    kind: &str,
    versions: &[String],
) -> Result<u64, String> {
    if group == CORE_GROUP && UNCOUNTED_CORE_KINDS.contains(&kind) {
        return Err(format!(
            "`existingItems` is not reported: the catalog does not count `{kind}` items this way."
        ));
    }

    let unavailable =
        |error: ToolError| format!("`existingItems` could not be counted ({}).", error.code);

    let api_versions = versions
        .iter()
        .map(|version| {
            Ok(Predicate::Eq {
                field: FieldPath::new("apiVersion")?,
                value: QueryValue::string(&format!("{group}/{version}"))?,
            })
        })
        .collect::<Result<Vec<_>, ToolError>>()
        .map_err(unavailable)?;

    let by_kind = Predicate::Eq {
        field: FieldPath::new("kind").map_err(unavailable)?,
        value: QueryValue::string(kind).map_err(unavailable)?,
    };
    let predicate = Predicate::And(vec![by_kind, Predicate::Or(api_versions)]);

    let query = ListQuery {
        raw_query: predicate.encode_rawq().map_err(unavailable)?,
        ..ListQuery::default()
    };

    engine
        .count_items(&query)
        .await
        .map(|response| response.value.count)
        .map_err(|error| {
            tracing::warn!(
                kind,
                code = error.code,
                "the type's items could not be counted"
            );
            unavailable(error)
        })
}

#[cfg(test)]
mod tests;
