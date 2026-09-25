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
// DR-86 — `fields` mode: the schema of just the fields an agent is about to change.
//
// A patch changes a few fields; T8 turns it into a whole-item `PUT` the engine re-validates, but the
// untouched fields were valid already, so what the agent needs is the rules of the ones it touches.
// Returning those alone is not the silent truncation D34 forbids: the agent asks for a subset, knows
// it got one, and the whole definition is one call away.

use catalog_client::{Remedy, ToolError, error::codes};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// A JSON Schema's own keywords the walker descends through.
const PROPERTIES: &str = "properties";
const ITEMS: &str = "items";
const REF: &str = "$ref";
const DEFS: &str = "$defs";

/// The combinator whose branches all apply.
const ALL_OF: &str = "allOf";

/// The combinator whose branches may apply.
const ANY_OF: &str = "anyOf";

/// The combinators whose branches apply depending on the item.
const ALTERNATIVES: [&str; 2] = ["oneOf", ANY_OF];

/// Every combinator, for listing the fields that exist.
const COMBINATORS: [&str; 3] = [ALL_OF, "oneOf", ANY_OF];

/// The only reference form the shipped schemas use, and the one that stays valid when the
/// definitions it names are returned beside the fields under the same `$defs` key.
const LOCAL_DEFS_PREFIX: &str = "#/$defs/";

/// The schema of each requested field, and every `$defs` entry they reference.
pub struct FieldSchemas {
    /// Path → that field's schema, in the order asked.
    pub fields: Map<String, Value>,

    /// The definitions the fields reference, transitively — so every `#/$defs/<name>` in them
    /// resolves against the answer itself, and a recursive definition is carried once.
    pub defs: Map<String, Value>,
}

/// Extracts the schema of each of `paths` (`spec.lifecycle`, `spec.ports.name`, …) from `root`,
/// the selected version's `openAPIV31Schema`.
///
/// A path walks `properties`, steps into `items` when it crosses an array, follows `#/$defs/`
/// references, and looks through `allOf`/`oneOf`/`anyOf` branches. A field defined in several
/// places comes back composed as JSON Schema composes it (see [`child`]): nothing is chosen for
/// the model, because which alternative applies depends on the item.
///
/// # Errors
///
/// A path that names no field is `invalid_input`, naming the fields that do exist where the walk
/// stopped — the model's next attempt can then land.
pub fn extract(root: &Value, paths: &[String]) -> Result<FieldSchemas, ToolError> {
    let empty = Map::new();
    let defs = root.get(DEFS).and_then(Value::as_object).unwrap_or(&empty);
    let mut fields = Map::new();

    for path in paths {
        fields.insert(path.clone(), walk(root, defs, path)?);
    }

    let mut used = BTreeSet::new();
    for schema in fields.values() {
        collect_refs(schema, defs, &mut used);
    }

    Ok(FieldSchemas {
        fields,
        defs: used
            .into_iter()
            .filter_map(|name| defs.get(&name).map(|def| (name, def.clone())))
            .collect(),
    })
}

/// Follows one dotted path from the root.
fn walk(root: &Value, defs: &Map<String, Value>, path: &str) -> Result<Value, ToolError> {
    let mut node = root.clone();
    let mut walked = String::new();

    for segment in path.split('.') {
        node = child(&node, defs, segment, &mut Vec::new())
            .ok_or_else(|| unknown_field(path, &walked, &node, defs))?;

        if !walked.is_empty() {
            walked.push('.');
        }
        walked.push_str(segment);
    }

    Ok(node)
}

/// The schema `segment` has below `node`, composed the way JSON Schema composes it — or `None`
/// when nothing below `node` defines it.
///
/// What **always** applies — `node`'s own property (or, for an array, its `items`' one), every
/// `allOf` branch's, and the definition a `$ref` points at — is combined as an `allOf`. What
/// applies **depending on the item** — the `oneOf`/`anyOf` branches that define it — is an
/// `anyOf`: at the field's level that is the honest reading, because two branches whose objects
/// exclude each other may still allow the same value for this one field. A property declared on
/// the object and narrowed per branch — a discriminated union — therefore comes back as
/// `{"allOf": [own, {"anyOf": [branches…]}]}`, never as the looser union of all of them.
///
/// A `$ref` is followed **beside** the node's other keywords, not instead of them — JSON Schema
/// 2020-12 lets the two sit together. `chain` holds the references followed since the last path
/// segment was consumed: one reappearing is a cycle that names nothing new, so it stops there
/// instead of recursing without end.
fn child(
    node: &Value,
    defs: &Map<String, Value>,
    segment: &str,
    chain: &mut Vec<String>,
) -> Option<Value> {
    let mut always = Vec::new();

    match node
        .get(PROPERTIES)
        .and_then(|properties| properties.get(segment))
    {
        Some(own) => always.push(own.clone()),
        None => always.extend(
            node.get(ITEMS)
                .and_then(|items| child(items, defs, segment, chain)),
        ),
    }

    for branch in branches(node, ALL_OF) {
        always.extend(child(branch, defs, segment, chain));
    }

    follow(node, defs, chain, |def, chain| {
        always.extend(child(def, defs, segment, chain));
    });

    let mut depending = Vec::new();
    for combinator in ALTERNATIVES {
        for branch in branches(node, combinator) {
            depending.extend(child(branch, defs, segment, chain));
        }
    }
    always.extend(combine(ANY_OF, depending));

    combine(ALL_OF, always)
}

/// `schemas` as one: nothing, the only one, or all of them under `combinator` — each distinct
/// schema once, since two routes to the same definition are one constraint.
fn combine(combinator: &str, schemas: Vec<Value>) -> Option<Value> {
    let mut distinct: Vec<Value> = Vec::new();
    for schema in schemas {
        if !distinct.contains(&schema) {
            distinct.push(schema);
        }
    }

    match distinct.len() {
        0 => None,
        1 => distinct.pop(),
        _ => Some(json!({ combinator: distinct })),
    }
}

/// The branches of `node`'s `combinator`.
fn branches<'a>(node: &'a Value, combinator: &str) -> impl Iterator<Item = &'a Value> {
    node.get(combinator)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The `$defs` name `node`'s local reference points at, if it has one.
fn local_ref(node: &Value) -> Option<&str> {
    node.get(REF)
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix(LOCAL_DEFS_PREFIX))
}

/// Calls `visit` with the definition `node` references, unless following it would close a cycle
/// in `chain`.
fn follow(
    node: &Value,
    defs: &Map<String, Value>,
    chain: &mut Vec<String>,
    mut visit: impl FnMut(&Value, &mut Vec<String>),
) {
    if let Some(name) = local_ref(node)
        && !chain.iter().any(|followed| followed == name)
        && let Some(def) = defs.get(name)
    {
        chain.push(name.to_string());
        visit(def, chain);
        chain.pop();
    }
}

/// The names of every `$defs` entry `schema` references, following them transitively and visiting
/// each once, so a recursive definition terminates.
fn collect_refs(schema: &Value, defs: &Map<String, Value>, used: &mut BTreeSet<String>) {
    match schema {
        Value::Object(object) => {
            if let Some(name) = local_ref(schema)
                && used.insert(name.to_string())
                && let Some(def) = defs.get(name)
            {
                collect_refs(def, defs, used);
            }

            for value in object.values() {
                collect_refs(value, defs, used);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_refs(value, defs, used);
            }
        }
        _ => {}
    }
}

/// The error for a path that names nothing, listing what does exist where the walk stopped.
fn unknown_field(path: &str, walked: &str, node: &Value, defs: &Map<String, Value>) -> ToolError {
    let mut valid: BTreeSet<String> = BTreeSet::new();
    collect_property_names(node, defs, &mut valid, &mut Vec::new());

    let prefix = if walked.is_empty() {
        String::new()
    } else {
        format!("{walked}.")
    };
    let valid: Vec<String> = valid
        .into_iter()
        .map(|name| format!("{prefix}{name}"))
        .collect();

    ToolError::new(
        codes::INVALID_INPUT,
        Remedy::RetryAfterChange,
        format!("`{path}` is not a field of this type."),
    )
    .with_details(json!({ "field": "fields", "path": path, "validFields": valid }))
}

/// The property names reachable directly below `node`, the way [`children`] would find them.
fn collect_property_names(
    node: &Value,
    defs: &Map<String, Value>,
    names: &mut BTreeSet<String>,
    chain: &mut Vec<String>,
) {
    if let Some(properties) = node.get(PROPERTIES).and_then(Value::as_object) {
        names.extend(properties.keys().cloned());
    } else if let Some(items) = node.get(ITEMS) {
        collect_property_names(items, defs, names, chain);
    }

    for combinator in COMBINATORS {
        for branch in branches(node, combinator) {
            collect_property_names(branch, defs, names, chain);
        }
    }

    follow(node, defs, chain, |def, chain| {
        collect_property_names(def, defs, names, chain);
    });
}
