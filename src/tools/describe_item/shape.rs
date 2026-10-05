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
// The relationship shaper and the client-side grouping. This is where the tool's size comes from: a
// relationship on the wire is ~1 KB of BFF routing data, and the agent needs a handful of its fields.

use catalog_client::models::{ItemRelationshipEntry, RelationshipDirection};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The grouping key of a relationship whose record carries no `typeRef`, which the engine does
/// not produce — named so the entry is still grouped rather than dropped.
const UNKNOWN_TYPE: &str = "unknown";

/// How the shaped relationships are grouped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    /// `{"outbound": [...], "inbound": [...]}`; `type` carried per entry.
    #[default]
    ByDirection,

    /// `{"<type>": [...]}`; `direction` carried per entry.
    ByType,
}

/// One shaped relationship, in its serialised field order.
///
/// A resolved entry is `{name, kind, group, title?, type|direction}`; an unresolved one is `{urn,
/// type|direction, unresolved: true}`. Whichever of `type` and `direction` is the grouping
/// key is left out of the entry: it would be repeated, and always redundant.
#[derive(Serialize)]
struct ShapedEntry {
    #[serde(rename = "name", skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,

    /// The related item's group. A kind is unique only within a group, so `name` and `kind` alone
    /// cannot address the other end when several types share the kind; with `group` the entry is
    /// exactly what a follow-up `describe_item` call needs.
    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    group: Option<String>,

    /// The related item's human-readable title, when it has one. Names are often generated ids,
    /// so the title is what tells the reader which related item matters; the engine already
    /// sends it with the related item, so it costs no extra call.
    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    title: Option<String>,

    #[serde(rename = "urn", skip_serializing_if = "Option::is_none")]
    urn: Option<String>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    relationship_type: Option<String>,

    #[serde(rename = "direction", skip_serializing_if = "Option::is_none")]
    direction: Option<&'static str>,

    #[serde(rename = "unresolved", skip_serializing_if = "Option::is_none")]
    unresolved: Option<bool>,
}

/// The relationship type's name: the last segment of `spec.typeRef`, not the whole URN.
pub fn relationship_type(entry: &ItemRelationshipEntry) -> String {
    entry
        .type_ref()
        .and_then(|urn| urn.rsplit(':').next())
        .filter(|name| !name.is_empty())
        .unwrap_or(UNKNOWN_TYPE)
        .to_string()
}

/// Shapes one entry for `grouping`.
///
/// **An entry whose other end is unresolved is reported, never dropped**: an omitted
/// relationship reads as *"no such connection"*, a stronger and wronger claim than *"I could not
/// resolve it"*. `unresolved` states what happened to the response and nothing about why — the
/// causes cannot be told apart from here, so the tool never says "deleted".
fn shape(entry: &ItemRelationshipEntry, grouping: Grouping) -> Value {
    let (relationship_type, direction) = match grouping {
        Grouping::ByDirection => (Some(relationship_type(entry)), None),
        Grouping::ByType => (None, Some(entry.direction.as_str())),
    };

    let shaped = match &entry.related_item {
        Some(related) => ShapedEntry {
            name: Some(related.metadata.name.clone()),
            kind: Some(related.kind.clone()),
            group: related
                .api_version
                .split_once('/')
                .map(|(group, _)| group.to_string()),
            title: related.metadata.title.clone(),
            urn: None,
            relationship_type,
            direction,
            unresolved: None,
        },
        None => ShapedEntry {
            name: None,
            kind: None,
            group: None,
            title: None,
            urn: entry.other_end().map(str::to_string),
            relationship_type,
            direction,
            unresolved: Some(true),
        },
    };

    // A struct of strings and booleans cannot fail to serialise; the fallback exists only so the
    // entry is still counted rather than silently lost.
    serde_json::to_value(shaped).unwrap_or(Value::Null)
}

/// Groups the shaped entries: both groupings come from the same flat, `groupBy`-free
/// listing.
///
/// By direction, an empty group is `[]` only when it is known to be empty: both keys are present
/// on a complete listing, but a key is left out when the caller restricted the listing to the
/// other `direction` — `inbound: []` would claim there are no inbound relationships when none were
/// asked for — and when `truncated` says more pages follow and this page holds none of that
/// direction, since its entries may simply not have fitted. By type, keys are sorted, so the
/// payload is deterministic whatever order the engine used.
pub fn group(
    entries: &[ItemRelationshipEntry],
    grouping: Grouping,
    only: Option<RelationshipDirection>,
    truncated: bool,
) -> Value {
    match grouping {
        Grouping::ByDirection => {
            let mut groups = Map::new();

            for direction in [
                RelationshipDirection::Outbound,
                RelationshipDirection::Inbound,
            ] {
                if only.is_some_and(|only| only != direction) {
                    continue;
                }

                let shaped: Vec<Value> = entries
                    .iter()
                    .filter(|entry| entry.direction == direction)
                    .map(|entry| shape(entry, grouping))
                    .collect();

                if truncated && shaped.is_empty() {
                    continue;
                }

                groups.insert(direction.as_str().to_string(), Value::Array(shaped));
            }

            Value::Object(groups)
        }
        Grouping::ByType => {
            let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();

            for entry in entries {
                groups
                    .entry(relationship_type(entry))
                    .or_default()
                    .push(shape(entry, grouping));
            }

            Value::Object(
                groups
                    .into_iter()
                    .map(|(key, shaped)| (key, Value::Array(shaped)))
                    .collect(),
            )
        }
    }
}
