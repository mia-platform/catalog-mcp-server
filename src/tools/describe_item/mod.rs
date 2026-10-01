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
    tools::{arguments::validate_group, lookup::resolve},
};
use catalog_client::{
    ItemAddress, Remedy, ToolError,
    error::{cancelled, codes},
    is_valid_kind,
    models::RelationshipDirection,
    ops::relationships::RelationshipQuery,
    pagination::{DEFAULT_LIMIT, EngineCursor, MAX_LIMIT, ToolCursor, fingerprint},
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The four-field shaper and the grouping.
mod shape;

use shape::Grouping;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "describe_item";

/// What the tool does.
const TOOL_DESCRIPTION: &str = "Describes one catalog item: what it is, and what it is connected \
     to. Give the item's name; `kind` only if the name is ambiguous. Returns the item together with \
     its relationships, so you do not need a second call to find out what depends on it.";

/// The longest `name`, in bytes.
pub const MAX_NAME_BYTES: usize = 256;

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// The smallest relationship page. The engine's own minimum.
const MIN_RELATIONSHIP_LIMIT: u32 = 1;

/// Default for the two `include_*` flags: the one-round-trip answer is the point.
fn default_true() -> bool {
    true
}

// Which end of its relationships the item is: `inbound` when it is the target, `outbound` when it
// is the source.
//
// The two argument enums are documented with `//`, not `///`, on purpose: `schemars` copies doc
// comments on an enum and its variants into the input schema — a `oneOf` with a description per
// variant, restating what the field's own description already says. That tripled these two
// properties (~310 B each against ~120 B as a plain `enum`) on a payload every conversation pays
// for.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum Direction {
    #[serde(rename = "inbound")]
    Inbound,

    #[serde(rename = "outbound")]
    Outbound,
}

impl From<Direction> for RelationshipDirection {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Inbound => Self::Inbound,
            Direction::Outbound => Self::Outbound,
        }
    }
}

// How relationships are grouped: by `direction` (`outbound` and `inbound`) or by relationship
// `type`. Documented with `//` for the reason given on `Direction`.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum GroupBy {
    #[serde(rename = "direction")]
    Direction,

    #[serde(rename = "type")]
    Type,
}

/// Arguments for `describe_item`.
///
/// `direction` and `group_by: "direction"` are **both legal together** — one filters, the other
/// groups. The two enums are typed, so a bad value is `invalid_arguments` before the tool runs.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct DescribeItemInput {
    /// The item's name.
    #[serde(rename = "name")]
    pub name: String,

    /// Only needed when the name is ambiguous.
    #[serde(rename = "kind")]
    pub kind: Option<String>,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// Return the item's spec. Default true.
    #[serde(rename = "include_spec", default = "default_true")]
    pub include_spec: bool,

    /// Return the item's relationships. Default true.
    #[serde(rename = "include_relationships", default = "default_true")]
    pub include_relationships: bool,

    /// Restrict relationships to one direction.
    #[serde(rename = "direction")]
    pub direction: Option<Direction>,

    /// Group relationships by direction (default) or by relationship type.
    #[serde(rename = "group_by")]
    pub group_by: Option<GroupBy>,

    /// Maximum relationships returned. Default 50, clamped to 200.
    #[serde(rename = "relationship_limit")]
    pub relationship_limit: Option<u16>,

    /// Opaque continuation for relationships only.
    #[serde(rename = "relationship_cursor")]
    pub relationship_cursor: Option<String>,
}

/// The whole response, in its serialised order.
#[derive(Serialize)]
struct DescribeItemOutput {
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

    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    title: Option<String>,

    #[serde(rename = "labels", skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,

    /// Omitted when `include_spec` is false.
    #[serde(rename = "spec", skip_serializing_if = "Option::is_none")]
    spec: Option<Value>,

    /// Omitted when the item has none. Present because `patch_item_custom_fields` is the only
    /// tool that writes them, and the model must not patch a map it has never seen.
    #[serde(rename = "customFields", skip_serializing_if = "Option::is_none")]
    custom_fields: Option<Value>,

    /// Omitted when not asked for; **`null` when the fetch failed**, which is how it is
    /// told apart from `{}` meaning *none*.
    #[serde(rename = "relationships", skip_serializing_if = "Option::is_none")]
    relationships: Option<Value>,

    /// Whether the engine had more than one page — the caller's lever, not a runtime cap.
    #[serde(
        rename = "relationshipsTruncated",
        skip_serializing_if = "Option::is_none"
    )]
    relationships_truncated: Option<bool>,

    /// Absent when there are no more relationships.
    #[serde(rename = "relationshipCursor", skip_serializing_if = "Option::is_none")]
    relationship_cursor: Option<String>,

    /// The effective page size, **only** when the clamp changed it, as in `search_catalog`.
    #[serde(rename = "relationshipLimit", skip_serializing_if = "Option::is_none")]
    relationship_limit: Option<u32>,
}

/// What a relationship cursor pins: where the item lives, so a later page resolves nothing again.
#[derive(Serialize, Deserialize)]
struct RelationshipsPinned {
    #[serde(rename = "g")]
    group: String,

    #[serde(rename = "v")]
    version: String,

    #[serde(rename = "f")]
    family: String,
}

/// `describe_item` — one item, what it is and what it is connected to, in one call.
///
/// Resolution — from `kind` through `catalog-client`'s point lookup, or from the name alone through
/// a two-row probe — then the item and its relationships **concurrently**, then the shaper.
/// A failed relationships call degrades the answer rather than failing it.
pub struct DescribeItem;

impl Tool for DescribeItem {
    type Input = DescribeItemInput;

    /// `readOnlyHint: true`; every other hint is the specification's default.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<DescribeItemInput>(
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

        let (limit, limit_echo) = effective_limit(input.relationship_limit);
        let direction = input.direction.map(RelationshipDirection::from);
        let grouping = match input.group_by {
            Some(GroupBy::Type) => Grouping::ByType,
            Some(GroupBy::Direction) | None => Grouping::ByDirection,
        };
        let cursor_fingerprint = relationships_fingerprint(
            &input.name,
            input.kind.as_deref(),
            input.group.as_deref(),
            direction,
        );
        let engine = context.engine();

        let (address, engine_cursor) = match &input.relationship_cursor {
            Some(raw) => resume(raw, &cursor_fingerprint, &input.name)?,
            None => (
                resolve(
                    engine,
                    TOOL_NAME,
                    &input.name,
                    input.kind.as_deref(),
                    input.group.as_deref(),
                )
                .await?,
                None,
            ),
        };

        // The item and its relationships are independent, so they are fetched at once,
        // both under the call's deadline, and the whole fan-out stops if the caller goes away.
        let fetch = async {
            if input.include_relationships {
                let query = RelationshipQuery {
                    limit: Some(limit),
                    cursor: engine_cursor,
                    direction,
                };
                let (item, relationships) = tokio::join!(
                    engine.get_item(&address),
                    engine.get_relationships(&address, &query)
                );

                (item, Some(relationships))
            } else {
                (engine.get_item(&address).await, None)
            }
        };

        let (item, relationships) = tokio::select! {
            biased;
            () = context.cancellation().cancelled() => return Err(cancelled()),
            outcome = fetch => outcome,
        };

        // Without the item there is nothing to describe, so its failure is the call's.
        let item = item?.value;

        let mut warning = None;
        let (relationships, truncated, next_cursor) = match relationships {
            None => (None, None, None),
            Some(Ok(page)) => {
                let next_cursor = page
                    .value
                    .next
                    .as_ref()
                    .map(|next| mint(next, &cursor_fingerprint, &address))
                    .transpose()?;

                (
                    Some(shape::group(&page.value.items, grouping, direction)),
                    Some(page.value.next.is_some()),
                    next_cursor,
                )
            }
            // A secondary fetch failing degrades the answer; it does not remove it.
            Some(Err(error)) => {
                if error.code == codes::SERVER_DEFECT {
                    tracing::error!(
                        item = %address,
                        "the catalog rejected a relationships request this server built"
                    );
                }
                warning = Some(format!(
                    "The item's relationships could not be read ({}): {} The item itself is \
                     complete.",
                    error.code, error.message
                ));

                (Some(Value::Null), None, None)
            }
        };

        let output = DescribeItemOutput {
            name: item.metadata.name,
            kind: item.kind,
            group: address.group().to_string(),
            version: address.version().to_string(),
            family: address.family().to_string(),
            title: item.metadata.title,
            labels: item.metadata.labels,
            spec: input.include_spec.then_some(item.spec),
            custom_fields: item.custom_fields.filter(has_entries),
            relationships,
            relationships_truncated: truncated,
            relationship_cursor: next_cursor,
            relationship_limit: limit_echo.filter(|_| input.include_relationships),
        };

        let payload = serde_json::to_value(&output).map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The item could not be rendered: {err}"),
            )
        })?;

        let output = ToolOutput::new(payload);
        Ok(match warning {
            Some(warning) => output.with_warning(warning),
            None => output,
        })
    }
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &DescribeItemInput) -> Result<(), ToolError> {
    if input.name.is_empty() || input.name.len() > MAX_NAME_BYTES {
        return Err(invalid(
            "name",
            format!("`name` must be between 1 and {MAX_NAME_BYTES} bytes."),
        ));
    }

    if let Some(kind) = &input.kind {
        if kind.len() > MAX_KIND_BYTES {
            return Err(invalid(
                "kind",
                format!(
                    "`kind` is {} bytes; at most {MAX_KIND_BYTES} are accepted.",
                    kind.len()
                ),
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

/// The relationship page size, and what to echo, by the same rule as `search_catalog`'s: clamped
/// into the engine's range, echoed only when the clamp changed it.
fn effective_limit(requested: Option<u16>) -> (u32, Option<u32>) {
    let Some(requested) = requested else {
        return (DEFAULT_LIMIT, None);
    };

    let requested = u32::from(requested);
    let effective = requested.clamp(MIN_RELATIONSHIP_LIMIT, MAX_LIMIT);

    (effective, (effective != requested).then_some(effective))
}

/// Whether a `customFields` value is worth returning — present and not an empty map.
fn has_entries(value: &Value) -> bool {
    !matches!(value, Value::Null) && value.as_object().is_none_or(|map| !map.is_empty())
}

/// The fingerprint a relationship cursor is minted and checked against: the item it belongs to
/// and the one query parameter that shapes the engine's listing. `group_by` and the page size are
/// ours to apply, so a later page may change them.
fn relationships_fingerprint(
    name: &str,
    kind: Option<&str>,
    group: Option<&str>,
    direction: Option<RelationshipDirection>,
) -> String {
    fingerprint(&json!({
        "name": name,
        "kind": kind,
        "group": group,
        "direction": direction.map(RelationshipDirection::as_str),
    }))
}

/// Mints the cursor for the next page of relationships.
fn mint(
    next: &EngineCursor,
    fingerprint: &str,
    address: &ItemAddress,
) -> Result<String, ToolError> {
    ToolCursor::new(
        Some(next),
        fingerprint,
        RelationshipsPinned {
            group: address.group().to_string(),
            version: address.version().to_string(),
            family: address.family().to_string(),
        },
    )
    .encode()
}

/// Decodes a relationship cursor into the item's address and the engine's continuation.
///
/// A cursor for another item or another `direction`, or one that does not decode, is
/// `invalid_cursor` — never read as "no more relationships".
fn resume(
    raw: &str,
    fingerprint: &str,
    name: &str,
) -> Result<(ItemAddress, Option<EngineCursor>), ToolError> {
    let cursor = ToolCursor::<RelationshipsPinned>::decode(raw, fingerprint)?;
    let engine = cursor.engine_cursor().ok_or_else(unusable_cursor)?;
    let pinned = cursor.pinned;
    let address = ItemAddress::new(pinned.group, pinned.version, pinned.family, name)
        .map_err(|_| unusable_cursor())?;

    Ok((address, Some(engine)))
}

/// A cursor that decoded but cannot be continued from.
fn unusable_cursor() -> ToolError {
    ToolError::new(
        codes::INVALID_CURSOR,
        Remedy::RetryAfterChange,
        "That cursor cannot be continued from.",
    )
    .with_next_step("describe the item again without a relationship_cursor")
}

#[cfg(test)]
mod tests;
