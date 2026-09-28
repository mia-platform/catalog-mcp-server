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

/// The `required` keyword.
const REQUIRED_KEY: &str = "required";

/// The `anyOf` keyword.
const ANY_OF_KEY: &str = "anyOf";

/// The JSON Schema `null` type.
const NULL_TYPE: &str = "null";

/// The `format` keyword.
const FORMAT_KEY: &str = "format";

/// The `minimum` keyword.
const MINIMUM_KEY: &str = "minimum";

/// The `maximum` keyword.
const MAXIMUM_KEY: &str = "maximum";

/// The integer formats `schemars` emits for Rust's integer types, with each type's own range —
/// which is all a `minimum`/`maximum` beside them restates.
const INTEGER_FORMATS: [(&str, i128, i128); 8] = [
    ("uint8", 0, u8::MAX as i128),
    ("uint16", 0, u16::MAX as i128),
    ("uint32", 0, u32::MAX as i128),
    ("uint64", 0, u64::MAX as i128),
    ("int8", i8::MIN as i128, i8::MAX as i128),
    ("int16", i16::MIN as i128, i16::MAX as i128),
    ("int32", i32::MIN as i128, i32::MAX as i128),
    ("int64", i64::MIN as i128, i64::MAX as i128),
];

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
/// 5. Keys are sorted at every depth.
/// 6. An integer's `format` is dropped, with any `minimum`/`maximum` that only restates that Rust
///    type's range (`uint16` → `0…65535`): the storage width of the field says nothing about
///    what the tool accepts, which its description states (DR-49, F-10).
/// 7. An **optional** argument does not also declare `null`: `["string","null"]` becomes
///    `"string"`, and `anyOf: [X, {"type":"null"}]` becomes `X`. Being absent from `required`
///    already says the argument may be left out, which is what `Option` means here; serde still
///    accepts an explicit `null`, so no call that worked stops working (F-10).
///
/// A parameterless tool therefore minifies to `{"additionalProperties":false,"type":"object"}`.
///
/// Rule 5 is explicit because the workspace enables `serde_json`'s `preserve_order`, which makes
/// a tool's **output** follow its struct order. A schema must not: its bytes are pinned by a
/// golden and paid on every `tools/list`, and they should not move because `schemars` changed the
/// order it emits keys in.
pub fn minify_input_schema(schema: &Map<String, Value>) -> Map<String, Value> {
    let mut schema = schema.clone();

    schema.remove(SCHEMA_DIALECT_KEY);

    inline_single_use_defs(&mut schema);
    strip_nested_titles(&mut schema);
    strip_integer_formats(&mut schema);
    strip_optional_nulls(&mut schema);

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

    for value in schema.values_mut() {
        value.sort_all_objects();
    }
    schema.sort_keys();

    schema
}

/// Rule 6 — drops every integer `format`, with the bounds that only restate its type's range.
fn strip_integer_formats(schema: &mut Map<String, Value>) {
    let mut root = Value::Object(std::mem::take(schema));
    strip_integer_format(&mut root);

    if let Value::Object(root) = root {
        *schema = root;
    }
}

/// [`strip_integer_formats`] on one node and everything below it.
fn strip_integer_format(node: &mut Value) {
    match node {
        Value::Object(object) => {
            let range = object
                .get(FORMAT_KEY)
                .and_then(Value::as_str)
                .and_then(|format| {
                    INTEGER_FORMATS
                        .iter()
                        .find(|(name, _, _)| *name == format)
                        .map(|(_, min, max)| (*min, *max))
                });

            if let Some((min, max)) = range {
                object.remove(FORMAT_KEY);

                for (key, bound) in [(MINIMUM_KEY, min), (MAXIMUM_KEY, max)] {
                    let restates = object.get(key).and_then(|value| {
                        value
                            .as_i64()
                            .map(i128::from)
                            .or(value.as_u64().map(i128::from))
                    }) == Some(bound);
                    if restates {
                        object.remove(key);
                    }
                }
            }

            object.values_mut().for_each(strip_integer_format);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_integer_format),
        _ => {}
    }
}

/// Rule 7 — an optional root argument declares its own type, not also `null`.
fn strip_optional_nulls(schema: &mut Map<String, Value>) {
    let required: Vec<String> = schema
        .get(REQUIRED_KEY)
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let Some(properties) = schema
        .get_mut(PROPERTIES_KEY)
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    for (name, property) in properties.iter_mut() {
        if required.contains(name) {
            continue;
        }

        let Some(property) = property.as_object_mut() else {
            continue;
        };

        // `["string", "null"]` → `"string"`.
        if let Some(Value::Array(types)) = property.get(TYPE_KEY) {
            let kept: Vec<Value> = types
                .iter()
                .filter(|kind| kind.as_str() != Some(NULL_TYPE))
                .cloned()
                .collect();

            if kept.len() < types.len() {
                let replacement = match kept.as_slice() {
                    [only] => only.clone(),
                    _ => Value::Array(kept),
                };
                property.insert(TYPE_KEY.to_string(), replacement);
            }
        }

        // `anyOf: [X, {"type": "null"}]` → `X`, merged into the property beside its siblings.
        let null_branch = json_null_schema();
        let collapsible = property
            .get(ANY_OF_KEY)
            .and_then(Value::as_array)
            .filter(|branches| branches.len() == 2 && branches.contains(&null_branch))
            .and_then(|branches| branches.iter().find(|branch| **branch != null_branch))
            .and_then(Value::as_object)
            .cloned();

        if let Some(kept) = collapsible {
            property.remove(ANY_OF_KEY);
            for (key, value) in kept {
                property.entry(key).or_insert(value);
            }
        }
    }
}

/// `{"type": "null"}`, the branch rule 7 folds away.
fn json_null_schema() -> Value {
    let mut null = Map::new();
    null.insert(TYPE_KEY.to_string(), Value::String(NULL_TYPE.to_string()));
    Value::Object(null)
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
