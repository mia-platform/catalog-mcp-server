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
        lookup::{no_such_item, resolve},
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

/// What the tool does (T9 §3). The consequences are here because they are not guessable from the
/// name, and the model's summary to the user needs to carry them.
const TOOL_DESCRIPTION: &str = "Deletes a catalog item permanently. This also removes every \
     relationship connected to it, in both directions, and its revision history — other items will \
     lose their links to it. There is no undo. Use `describe_item` first if you need to see what \
     will be affected.";

/// The longest `name`, in bytes (T9 §3, as T3).
pub const MAX_NAME_BYTES: usize = 256;

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// How many relationships one count reads (T9-P1): **one** page, the engine's largest. The
/// relationships listing has no count endpoint and no total, so more than this is reported as a
/// lower bound rather than walked.
pub const RELATIONSHIP_COUNT_PAGE: u32 = MAX_LIMIT;

/// Arguments for `delete_item` (T9 §3, DR-80).
///
/// No `confirm` and no `dry_run` (T9-D5): confirmation belongs to the client, which the default
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

/// How many relationships the delete removed, as far as one page can tell (T9-P1).
#[derive(Serialize)]
#[serde(untagged)]
enum RelationshipCount {
    /// Every one there was: the page ended the listing.
    Exact(usize),

    /// More than one page's worth: `"200+"`.
    AtLeast(String),
}

/// What was deleted (T9 §5). The engine's warnings — the cascade's among them — are added by the
/// runtime, present and empty when there are none (D28).
#[derive(Serialize)]
struct Deleted {
    /// Always `true`: a delete that did not happen is an error, never `false` (T9 §5, §6).
    #[serde(rename = "deleted")]
    deleted: bool,

    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "kind")]
    kind: String,

    /// A kind is unique per group only (DR-80), so the group says which type the item was.
    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    title: Option<String>,

    /// Both directions, counted before the delete (T9-D6). `null` when the count failed.
    #[serde(rename = "relationshipsRemoved")]
    relationships_removed: Option<RelationshipCount>,
}

/// T9 · `delete_item` — delete one item, and say what went with it.
///
/// Not a passthrough for one reason: **a failed cascade returns `204`** (T9-D4). The engine's
/// warning reaches the model verbatim through the runtime. Around it: find the item without
/// guessing (T9-D7), read it for the concurrency token (T9-D1, T9-D2), count what the cascade will
/// remove (T9-D6), and delete — reporting a `409` rather than retrying it (T9-D3).
pub struct DeleteItem;

impl Tool for DeleteItem {
    type Input = DeleteItemInput;

    /// Every hint is the specification's default — `destructiveHint: true` included — so none is
    /// emitted (D16). Silence says "destructive".
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

        // T9-D7 — a name matching several items answers the candidates and deletes nothing.
        let address = resolve(
            engine,
            TOOL_NAME,
            &input.name,
            input.kind.as_deref(),
            input.group.as_deref(),
        )
        .await?;

        // T9-D1 — the pre-read: "wrong name" here is a `not_found` with near matches, and a
        // failure is `catalog_unavailable` with nothing deleted.
        let item = match engine.get_item(&address).await {
            Ok(response) => response.value,
            Err(error) if error.code == codes::NOT_FOUND => {
                return Err(no_such_item(engine, &input.name).await);
            }
            Err(error) => return Err(error),
        };

        // T9-D2 — never "delete whatever is there now".
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

        Ok(match count_warning {
            Some(warning) => output.with_warning(warning),
            None => output,
        })
    }
}

/// NFR-10 — T9 §3's bounds, checked before anything reaches the engine.
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

    validate_group(input.group.as_deref(), input.kind.is_some())
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// T9-D6, T9-P1 — the relationships the cascade will remove, both directions, from one page.
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

/// The delete's own failures, in T9's words (T9 §6).
///
/// A `409` is reported and **not** retried (T9-D3); a `404` after a successful pre-read means
/// another writer deleted the item in between, which is not the same as a wrong name. Anything
/// else — `unknown_outcome` included (D20) — is the core's, unchanged.
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
