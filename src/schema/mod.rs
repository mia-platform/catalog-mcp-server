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
use serde_json::{Map, Value};

/// JSON Schema dialect marker the SDK keeps and we drop — roughly 60 bytes per tool, paid on
/// every conversation, telling the model nothing it can act on.
const SCHEMA_DIALECT_KEY: &str = "$schema";

/// Human-readable name of a schema node. The SDK strips it at the root only; every nested one
/// is ours to remove.
const TITLE_KEY: &str = "title";

/// Where `schemars` puts shared subschemas.
const DEFS_KEY: &str = "$defs";

/// A reference to a `$defs` entry.
const REF_KEY: &str = "$ref";

/// Prefix of every `$defs` reference `schemars` emits.
const DEFS_REF_PREFIX: &str = "#/$defs/";

/// The `type` keyword.
const TYPE_KEY: &str = "type";

/// The `properties` keyword.
const PROPERTIES_KEY: &str = "properties";

/// The `additionalProperties` keyword, set to `false` at the root so the model is told that an
/// invented argument is an error rather than discovering it at call time.
const ADDITIONAL_PROPERTIES_KEY: &str = "additionalProperties";

/// The only root `type` the specification allows for a tool input schema.
const OBJECT_TYPE: &str = "object";

/// Minifies a `schemars`-derived input schema into the form both `list_tools` and `get_tool`
/// serve (D17).
///
/// The SDK already strips the top-level `title` and `description`. Four things remain, and this
/// is the **one** place they are done, because `get_tool` feeds the SDK's `Mcp-Param-*`
/// validation and two sources would disagree silently:
///
/// 1. `$schema` is dropped — the SDK keeps it, proven by its own golden file.
/// 2. Every nested `title` is dropped.
/// 3. A `$defs` entry referenced exactly once is inlined at its `$ref` and removed.
/// 4. `additionalProperties: false` is set at the root.
///
/// A parameterless tool therefore minifies to `{"additionalProperties":false,"type":"object"}`.
pub fn minify_input_schema(schema: &Map<String, Value>) -> Map<String, Value> {
    let mut schema = schema.clone();

    schema.remove(SCHEMA_DIALECT_KEY);

    inline_single_use_defs(&mut schema);
    strip_nested_titles(&mut schema);

    schema.insert(TYPE_KEY.to_string(), Value::String(OBJECT_TYPE.to_string()));
    schema.insert(ADDITIONAL_PROPERTIES_KEY.to_string(), Value::Bool(false));

    // An empty `properties` object costs bytes and says nothing a missing one does not.
    if schema
        .get(PROPERTIES_KEY)
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        schema.remove(PROPERTIES_KEY);
    }

    schema
}

/// Inlines every `$defs` entry that is referenced exactly once, then removes `$defs` when it is
/// left empty.
///
/// A definition used twice stays: inlining it would duplicate its bytes, which is the opposite
/// of the point.
fn inline_single_use_defs(schema: &mut Map<String, Value>) {
    let Some(defs) = schema.get(DEFS_KEY).and_then(Value::as_object).cloned() else {
        return;
    };

    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    count_refs(&Value::Object(schema.clone()), &mut counts);

    let single_use: std::collections::BTreeMap<String, Value> = defs
        .iter()
        .filter(|(name, _)| counts.get(*name).copied() == Some(1))
        .map(|(name, body)| (name.clone(), body.clone()))
        .collect();

    if single_use.is_empty() {
        return;
    }

    let mut root = Value::Object(std::mem::take(schema));
    // The definitions themselves may reference one another, so `$defs` is rewritten too.
    inline_refs(&mut root, &single_use);

    let Value::Object(mut root) = root else {
        // PANIC-free: `root` was built from an object immediately above and `inline_refs` never
        // changes a node's kind.
        return;
    };

    if let Some(Value::Object(remaining)) = root.get_mut(DEFS_KEY) {
        remaining.retain(|name, _| !single_use.contains_key(name));

        if remaining.is_empty() {
            root.remove(DEFS_KEY);
        }
    }

    *schema = root;
}

/// Counts `$ref` targets across the whole document, `$defs` included.
fn count_refs(node: &Value, counts: &mut std::collections::BTreeMap<String, usize>) {
    match node {
        Value::Object(object) => {
            for (key, value) in object {
                if key == REF_KEY
                    && let Some(name) = value.as_str().and_then(|r| r.strip_prefix(DEFS_REF_PREFIX))
                {
                    *counts.entry(name.to_string()).or_default() += 1;
                }

                count_refs(value, counts);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| count_refs(item, counts)),
        _ => {}
    }
}

/// Replaces every `{"$ref": "#/$defs/<name>"}` whose `<name>` is in `bodies` with that body.
fn inline_refs(node: &mut Value, bodies: &std::collections::BTreeMap<String, Value>) {
    match node {
        Value::Object(object) => {
            let target = object
                .get(REF_KEY)
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix(DEFS_REF_PREFIX))
                .and_then(|name| bodies.get(name).map(|body| (name.to_string(), body)));

            if let Some((_, body)) = target {
                // A sibling keyword next to `$ref` (a `description` on the property, say) is
                // kept: the inlined body supplies the rest.
                let mut merged = body.as_object().cloned().unwrap_or_default();
                object.remove(REF_KEY);

                for (key, value) in object.iter() {
                    merged.insert(key.clone(), value.clone());
                }

                *node = Value::Object(merged);

                // The body just inlined may itself carry references.
                inline_refs(node, bodies);
                return;
            }

            object
                .values_mut()
                .for_each(|value| inline_refs(value, bodies));
        }
        Value::Array(items) => items.iter_mut().for_each(|item| inline_refs(item, bodies)),
        _ => {}
    }
}

/// Removes every `title` below the root.
fn strip_nested_titles(schema: &mut Map<String, Value>) {
    for value in schema.values_mut() {
        strip_titles(value);
    }
}

/// Removes every `title` in the subtree.
fn strip_titles(node: &mut Value) {
    match node {
        Value::Object(object) => {
            object.remove(TITLE_KEY);
            object.values_mut().for_each(strip_titles);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_titles),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
