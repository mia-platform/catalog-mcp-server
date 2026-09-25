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
// T3-D1, T3-D2, T3-D7 — the relationship shaper and the client-side grouping. This is where T3's
// size comes from: a relationship on the wire is ~1 KB of BFF routing data, and the agent needs
// four fields of it.

use catalog_client::models::{ItemRelationshipEntry, RelationshipDirection};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The grouping key of a relationship whose record carries no `typeRef`, which the engine does
/// not produce — named so the entry is still grouped rather than dropped.
const UNKNOWN_TYPE: &str = "unknown";

/// How the shaped relationships are grouped (T3 §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    /// `{"outbound": [...], "inbound": [...]}`; `type` carried per entry.
    #[default]
    ByDirection,

    /// `{"<type>": [...]}`; `direction` carried per entry.
    ByType,
}

/// One shaped relationship (T3-D2), in its serialised field order.
///
/// A resolved entry is `{name, kind, type|direction}`; an unresolved one is `{urn,
/// type|direction, unresolved: true}` (T3-D7). Whichever of `type` and `direction` is the grouping
/// key is left out of the entry: it would be repeated, and always redundant.
#[derive(Serialize)]
struct ShapedEntry {
    #[serde(rename = "name", skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
    kind: Option<String>,

    #[serde(rename = "urn", skip_serializing_if = "Option::is_none")]
    urn: Option<String>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    relationship_type: Option<String>,

    #[serde(rename = "direction", skip_serializing_if = "Option::is_none")]
    direction: Option<&'static str>,

    #[serde(rename = "unresolved", skip_serializing_if = "Option::is_none")]
    unresolved: Option<bool>,
}

/// The relationship type's name: the last segment of `spec.typeRef`, not the whole URN (T3-D2).
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
/// **An entry whose other end is unresolved is reported, never dropped** (T3-D7): an omitted
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
            urn: None,
            relationship_type,
            direction,
            unresolved: None,
        },
        None => ShapedEntry {
            name: None,
            kind: None,
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

/// Groups the shaped entries (T3-D1): both groupings come from the same flat, `groupBy`-free
/// listing.
///
/// By direction, both keys are always present — an empty group is `[]`, not absent — unless the
/// caller restricted the listing to one `direction`, in which case only that key appears: an
/// `inbound: []` would then claim there are no inbound relationships when none were asked for. By
/// type, keys are sorted, so the payload is deterministic whatever order the engine used.
pub fn group(
    entries: &[ItemRelationshipEntry],
    grouping: Grouping,
    only: Option<RelationshipDirection>,
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

                let shaped = entries
                    .iter()
                    .filter(|entry| entry.direction == direction)
                    .map(|entry| shape(entry, grouping))
                    .collect();

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
