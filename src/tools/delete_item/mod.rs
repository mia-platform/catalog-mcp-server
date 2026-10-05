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
    tools::{
        arguments::validate_group,
        lookup::{Resolved, no_such_item, resolve},
    },
};
use catalog_client::{
    EngineClient, ItemAddress, Remedy, ToolError, error::codes, is_valid_kind, is_valid_name,
    ops::relationships::RelationshipQuery, pagination::MAX_LIMIT,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "delete_item";

/// What the tool does. The consequences are here because they are not guessable from the
/// name, and the model's summary to the user needs to carry them.
///
/// The last sentence is there because on the gateway path the engine lists only some of the
/// relationships a delete removes, so the count is stated as what it is — the ones `describe_item`
/// lists — and never as the total.
const TOOL_DESCRIPTION: &str = "Deletes a catalog item permanently. This also removes every \
     relationship connected to it, in both directions, and its revision history — other items will \
     lose their links to it. There is no undo. Use `describe_item` first if you need to see what \
     will be affected. `relationshipsRemoved` counts the ones `describe_item` lists; the delete \
     removes all of them. `kind` and `group` are needed only if `name` is ambiguous.";

/// The longest `name`, in bytes, as for `describe_item`.
pub const MAX_NAME_BYTES: usize = 256;

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// How many relationships one count reads: **one** page, the engine's largest. The
/// relationships listing has no count endpoint and no total, so more than this is reported as a
/// lower bound rather than walked.
pub const RELATIONSHIP_COUNT_PAGE: u32 = MAX_LIMIT;

/// Arguments for `delete_item`; `group` says which type a shared `kind` means.
///
/// No `confirm` and no `dry_run`: confirmation belongs to the client, which the default
/// `destructiveHint` drives, and `describe_item` already shows what a delete would affect.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct DeleteItemInput {
    /// The item's name.
    #[serde(rename = "name")]
    pub name: String,

    /// Only needed when the name is ambiguous across types.
    #[serde(rename = "kind")]
    pub kind: Option<String>,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,
}

/// How many relationships the delete removed, as far as one page can tell.
#[derive(Serialize)]
#[serde(untagged)]
enum RelationshipCount {
    /// Every one there was: the page ended the listing.
    Exact(usize),

    /// More than one page's worth: `"200+"`.
    AtLeast(String),
}

/// What was deleted. The engine's warnings — the cascade's among them — are added by the runtime,
/// present and empty when there are none.
#[derive(Serialize)]
struct Deleted {
    /// Always `true`: a delete that did not happen is an error, never `false`.
    #[serde(rename = "deleted")]
    deleted: bool,

    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "kind")]
    kind: String,

    /// A kind is unique per group only, so the group says which type the item was.
    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    title: Option<String>,

    /// Both directions, counted before the delete — the relationships the listing shows this
    /// caller, which on the gateway path can be fewer than the delete removes. `null` when the
    /// count failed.
    #[serde(rename = "relationshipsRemoved")]
    relationships_removed: Option<RelationshipCount>,
}

/// `delete_item` — delete one item, and say what went with it.
///
/// Not a passthrough for one reason: **a failed cascade returns `204`**. The engine's warning
/// reaches the model verbatim through the runtime. Around it: find the item without guessing, read
/// it for the concurrency token, count what the cascade will remove, and delete — reporting a `409`
/// rather than retrying it.
pub struct DeleteItem;

impl Tool for DeleteItem {
    type Input = DeleteItemInput;

    /// Every hint is the specification's default — `destructiveHint: true` included — so none is
    /// emitted. Silence says "destructive".
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<DeleteItemInput>(TOOL_NAME, TOOL_DESCRIPTION, ToolAnnotations::new())
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;
        let engine = context.engine();

        // A name matching several items answers the candidates and deletes nothing.
        let Resolved { address, note } = resolve(
            engine,
            TOOL_NAME,
            &input.name,
            input.kind.as_deref(),
            input.group.as_deref(),
        )
        .await?;

        // The pre-read: "wrong name" here is a `not_found` with near matches, and a
        // failure is `catalog_unavailable` with nothing deleted.
        let item = match engine.get_item(&address).await {
            Ok(response) => response.value,
            Err(error) if error.code == codes::NOT_FOUND => {
                return Err(no_such_item(engine, &input.name).await);
            }
            Err(error) => return Err(error),
        };

        // Never "delete whatever is there now".
        let Some(resource_version) = item.resource_version.as_deref() else {
            tracing::error!(%address, "the catalog returned an item without a resourceVersion");
            return Err(ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                "The catalog returned the item without the version this server needs to delete \
                 it safely, so nothing was deleted.",
            ));
        };

        let (relationships_removed, count_warning) = count_relationships(engine, &address).await;

        engine
            .delete_item(&address, Some(resource_version))
            .await
            .map_err(|error| delete_error(error, &input.name))?;

        let deleted = Deleted {
            deleted: true,
            name: item.metadata.name,
            kind: item.kind,
            group: address.group().to_string(),
            title: item.metadata.title,
            relationships_removed,
        };

        let output = serde_json::to_value(deleted)
            .map(ToolOutput::new)
            .map_err(|err| {
                ToolError::new(
                    codes::SERVER_DEFECT,
                    Remedy::Escalate,
                    format!("The delete happened, but its report could not be rendered: {err}"),
                )
            })?;

        Ok(note
            .into_iter()
            .chain(count_warning)
            .fold(output, ToolOutput::with_warning))
    }
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &DeleteItemInput) -> Result<(), ToolError> {
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

    if let Some(kind) = &input.kind {
        if kind.is_empty() || kind.len() > MAX_KIND_BYTES {
            return Err(invalid(
                "kind",
                format!("`kind` must be between 1 and {MAX_KIND_BYTES} bytes."),
            ));
        }

        if !is_valid_kind(kind) {
            return Err(invalid(
                "kind",
                format!(
                    "`{kind}` is not a kind: a kind is a letter followed by letters and digits."
                ),
            )
            .with_next_step("call list_catalog_types to see the kinds that exist"));
        }
    }

    // `group` alone is usable here: it narrows the name to one group's items.
    validate_group(input.group.as_deref(), true)
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// The relationships the cascade will remove, both directions, from one page.
///
/// A failure does **not** stop the delete: the count is a report, not a precondition, and refusing
/// to delete because a report failed would make a read the gate on a write the caller asked for.
/// The answer then says the count is unknown, and why.
async fn count_relationships(
    engine: &EngineClient,
    address: &ItemAddress,
) -> (Option<RelationshipCount>, Option<String>) {
    let query = RelationshipQuery {
        limit: Some(RELATIONSHIP_COUNT_PAGE),
        ..RelationshipQuery::default()
    };

    match engine.get_relationships(address, &query).await {
        Ok(response) if response.value.next.is_some() => (
            Some(RelationshipCount::AtLeast(format!(
                "{RELATIONSHIP_COUNT_PAGE}+"
            ))),
            None,
        ),
        Ok(response) => (
            Some(RelationshipCount::Exact(response.value.items.len())),
            None,
        ),
        Err(error) => {
            tracing::warn!(%address, code = error.code, "relationships could not be counted");

            (
                None,
                Some(format!(
                    "The item's relationships could not be counted ({}), so \
                     `relationshipsRemoved` is null; the delete removed them all the same.",
                    error.code
                )),
            )
        }
    }
}

/// The delete's own failures, in this tool's words.
///
/// A `409` is reported and **not** retried; a `404` after a successful pre-read means
/// another writer deleted the item in between, which is not the same as a wrong name. Anything
/// else — `unknown_outcome` included — is `catalog-client`'s, unchanged.
fn delete_error(error: ToolError, name: &str) -> ToolError {
    match error.code {
        codes::CONFLICT => ToolError::new(
            codes::CONFLICT,
            Remedy::RetryLater,
            format!("`{name}` changed since it was read, so it was not deleted."),
        )
        .with_next_step(
            "call describe_item to see what changed, then delete again if it should still go",
        ),
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

#[cfg(test)]
mod tests;
