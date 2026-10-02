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
    EngineClient, FamilyAddress, FieldPath, ItemTypeAddress, Predicate, QueryValue, Remedy,
    ToolError, error::codes, find_item_type_document_or_suggest, is_valid_kind,
    models::ItemTypeDefinition, ops::ListQuery,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "delete_item_type";

/// What the tool does. The last sentence is what makes the two-phase flow discoverable
/// without a second tool.
const TOOL_DESCRIPTION: &str = "Deletes a catalog type definition and every item of that type, \
     along with their history and every relationship connected to them — including relationships \
     owned by items of other types. There is no undo. If the type has any items, the call is \
     refused the first time and reports how many would be destroyed.";

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// Arguments for `delete_item_type`; `group` says which type a shared `kind` means.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct DeleteItemTypeInput {
    /// The type's kind, e.g. "Service".
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// The number of items you expect to be destroyed. Required when the type has any items.
    #[serde(rename = "expected_items")]
    pub expected_items: Option<u64>,
}

/// The type was deleted.
#[derive(Serialize)]
struct Deleted {
    #[serde(rename = "deleted")]
    deleted: bool,

    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "name")]
    name: String,

    /// The count the guard checked a moment before the delete.
    #[serde(rename = "itemsDeleted")]
    items_deleted: u64,

    /// Always `null`: the engine reports no such number and none can be computed from here, and
    /// the description says so.
    #[serde(rename = "relationshipsDeleted")]
    relationships_deleted: Option<u64>,
}

/// The delete was refused for scope — **a success, not an error**, in the same shape.
#[derive(Serialize)]
struct Refused {
    #[serde(rename = "deleted")]
    deleted: bool,

    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "name")]
    name: String,

    /// How many items the delete would destroy, counted now.
    #[serde(rename = "itemsFound")]
    items_found: u64,

    /// The instruction, legible as the required second step.
    #[serde(rename = "action")]
    action: String,
}

/// `delete_item_type` — delete one type and everything of it, only once its scope is known.
///
/// The guard is `expected_items`: the items are counted **on every call**, and the delete is sent
/// only when there are none, or when the caller states exactly how many there are. A refusal is an
/// answer, not a failure; a `409` is reported, never retried; and a cascade that failed after a
/// `204` reaches the model through the engine's warning.
pub struct DeleteItemType;

impl Tool for DeleteItemType {
    type Input = DeleteItemTypeInput;

    /// Every hint is the specification's default — `destructiveHint: true` included — so none is
    /// emitted.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<DeleteItemTypeInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new(),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;
        let engine = context.engine();

        // An unknown kind comes back with near matches, never as an idempotent success.
        let document =
            find_item_type_document_or_suggest(engine, &input.kind, input.group.as_deref()).await?;
        let definition = &document.definition;
        let address = ItemTypeAddress::new(&definition.spec.group, &definition.spec.names.plural)?;

        let Some(resource_version) = document.raw.get("resourceVersion").and_then(Value::as_str)
        else {
            tracing::error!(%address, "the catalog returned a type without a resourceVersion");
            return Err(ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                "The catalog returned the type without the version this server needs to delete \
                 it safely, so nothing was deleted.",
            ));
        };

        // THE SCOPE: counted on every call, `expected_items` or not.
        let items = count_scope(engine, definition).await?;

        let proceed = match input.expected_items {
            Some(expected) => expected == items,
            None => items == 0,
        };

        if !proceed {
            return render(&Refused {
                deleted: false,
                kind: definition.spec.names.kind.clone(),
                group: address.group().to_string(),
                name: address.name().to_string(),
                items_found: items,
                action: format!("call again with expected_items: {items} to proceed"),
            });
        }

        engine
            .delete_item_type_definition(&address, Some(resource_version))
            .await
            .map_err(|error| delete_error(error, address.name()))?;

        render(&Deleted {
            deleted: true,
            kind: definition.spec.names.kind.clone(),
            group: address.group().to_string(),
            name: address.name().to_string(),
            items_deleted: items,
            relationships_deleted: None,
        })
    }
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &DeleteItemTypeInput) -> Result<(), ToolError> {
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

/// How many items the delete would destroy: every item of the kind under **every** version the type
/// declares — exactly what the engine's cascade selects (`kind = $1 AND api_version = ANY($2)`).
///
/// **Fails closed**. A served version is counted through its family count, which the
/// gateway gates on reading the type and does not filter: exact, or an error. Any error stops the
/// delete — a guard that cannot count must not pass. Only the items under versions that are no
/// longer served, which no family route reaches, are counted globally.
///
/// **Known limit, accepted.** Behind the gateway the global count covers only what the caller may
/// read, so items under an unserved version the caller cannot read are undercounted, while the
/// cascade still deletes them. It takes leftover items under a retired version *and* a caller
/// whose read access stops short of that version, which is rare. Refusing whenever a type has an
/// unserved version with items was rejected: the tool cannot tell a complete count from a short
/// one, so it would block safe deletes too. Only the engine, by reporting what its cascade
/// removed, could close the gap.
///
/// # Errors
///
/// Any failed count, as `catalog-client` mapped it, saying that nothing was deleted.
async fn count_scope(
    engine: &EngineClient,
    definition: &ItemTypeDefinition,
) -> Result<u64, ToolError> {
    let group = &definition.spec.group;
    let plural = &definition.spec.names.plural;
    let mut total = 0;
    let mut unserved = Vec::new();

    for version in &definition.spec.versions {
        if !version.served {
            unserved.push(version.name.as_str());
            continue;
        }

        let family = FamilyAddress::new(group, &version.name, plural)?;
        total += engine
            .count_family_items(&family, &ListQuery::default())
            .await
            .map_err(uncounted)?
            .value
            .count;
    }

    if !unserved.is_empty() {
        let api_versions = unserved
            .iter()
            .map(|version| {
                Ok(Predicate::Eq {
                    field: FieldPath::new("apiVersion")?,
                    value: QueryValue::string(&format!("{group}/{version}"))?,
                })
            })
            .collect::<Result<Vec<_>, ToolError>>()?;
        let predicate = Predicate::And(vec![
            Predicate::Eq {
                field: FieldPath::new("kind")?,
                value: QueryValue::string(&definition.spec.names.kind)?,
            },
            Predicate::Or(api_versions),
        ]);

        total += engine
            .count_items(&ListQuery {
                raw_query: predicate.encode_rawq()?,
                ..ListQuery::default()
            })
            .await
            .map_err(uncounted)?
            .value
            .count;
    }

    Ok(total)
}

/// A count that failed: `catalog-client`'s code and remedy, and the fact that nothing was deleted.
fn uncounted(error: ToolError) -> ToolError {
    ToolError {
        message: format!(
            "The type's items could not be counted, so nothing was deleted. {}",
            error.message
        ),
        ..error
    }
}

/// The delete's own failures, in this tool's words.
fn delete_error(error: ToolError, name: &str) -> ToolError {
    match error.code {
        // The intent was formed against the type as it was read.
        codes::CONFLICT => ToolError::new(
            codes::CONFLICT,
            Remedy::RetryLater,
            format!("`{name}` changed since it was read, so it was not deleted."),
        )
        .with_next_step(
            "call get_item_schema to see what changed, then delete again if it should still go",
        ),
        // It was found a moment ago: another writer deleted it in between.
        codes::NOT_FOUND => ToolError::new(
            codes::NOT_FOUND,
            Remedy::Escalate,
            format!(
                "`{name}` was deleted by someone else after this call read it; this call deleted \
                 nothing."
            ),
        )
        .with_details(json!({ "name": name })),
        _ => error,
    }
}

/// Serialises an answer, reporting the impossible failure as ours.
fn render<T: Serialize>(answer: &T) -> Result<ToolOutput, ToolError> {
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

#[cfg(test)]
mod tests;
