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
use crate::schema::minify_input_schema;
use rstest::rstest;
use serde_json::{Map, Value, json};

/// Minifies a literal schema, keeping the tests readable.
fn minify(schema: Value) -> Value {
    let object: Map<String, Value> = schema
        .as_object()
        .expect("a schema is a JSON object")
        .clone();

    Value::Object(minify_input_schema(&object))
}

/// D17 — `$schema` is ~60 bytes per tool that the SDK keeps and the model cannot act on.
#[rstest]
fn test_schema_dialect_is_dropped() {
    let minified = minify(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": { "name": { "type": "string" } }
    }));

    assert_eq!(minified.get("$schema"), None);
}

#[rstest]
fn test_nested_titles_are_dropped() {
    let minified = minify(json!({
        "type": "object",
        "properties": {
            "address": {
                "title": "Address",
                "type": "object",
                "properties": { "city": { "title": "City", "type": "string" } }
            }
        }
    }));

    assert_eq!(
        minified,
        json!({
            "additionalProperties": false,
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } }
                }
            }
        })
    );
}

/// Descriptions are **not** touched: they are what the model reads to use the tool correctly.
#[rstest]
fn test_nested_descriptions_are_kept() {
    let minified = minify(json!({
        "type": "object",
        "properties": { "kind": { "type": "string", "description": "The item kind." } }
    }));

    assert_eq!(
        minified["properties"]["kind"]["description"],
        json!("The item kind.")
    );
}

/// D17 — a `$defs` entry used once is bytes spent on indirection and nothing else.
#[rstest]
fn test_single_use_defs_are_inlined_and_removed() {
    let minified = minify(json!({
        "type": "object",
        "properties": { "target": { "$ref": "#/$defs/Target" } },
        "$defs": {
            "Target": { "type": "object", "properties": { "name": { "type": "string" } } }
        }
    }));

    assert_eq!(
        minified,
        json!({
            "additionalProperties": false,
            "type": "object",
            "properties": {
                "target": { "type": "object", "properties": { "name": { "type": "string" } } }
            }
        })
    );
}

/// A definition used twice stays: inlining it would duplicate its bytes, which is the opposite
/// of the point.
#[rstest]
fn test_multi_use_defs_are_kept() {
    let minified = minify(json!({
        "type": "object",
        "properties": {
            "from": { "$ref": "#/$defs/Ref" },
            "to": { "$ref": "#/$defs/Ref" }
        },
        "$defs": { "Ref": { "type": "string" } }
    }));

    assert_eq!(minified["$defs"]["Ref"], json!({ "type": "string" }));
    assert_eq!(minified["properties"]["from"]["$ref"], json!("#/$defs/Ref"));
}

/// A `$defs` entry that references another single-use entry still resolves.
#[rstest]
fn test_chained_single_use_defs_are_inlined() {
    let minified = minify(json!({
        "type": "object",
        "properties": { "outer": { "$ref": "#/$defs/Outer" } },
        "$defs": {
            "Outer": { "type": "object", "properties": { "inner": { "$ref": "#/$defs/Inner" } } },
            "Inner": { "type": "string" }
        }
    }));

    assert_eq!(minified.get("$defs"), None);
    assert_eq!(
        minified["properties"]["outer"]["properties"]["inner"],
        json!({ "type": "string" })
    );
}

/// A keyword sitting next to a `$ref` survives the inlining and wins over the body.
#[rstest]
fn test_siblings_of_an_inlined_ref_are_kept() {
    let minified = minify(json!({
        "type": "object",
        "properties": {
            "target": { "$ref": "#/$defs/Target", "description": "Where to write." }
        },
        "$defs": { "Target": { "type": "string" } }
    }));

    assert_eq!(
        minified["properties"]["target"],
        json!({ "type": "string", "description": "Where to write." })
    );
}

/// A self-referencing definition is used more than once by construction, so it is never
/// inlined and the minifier cannot loop on it.
#[rstest]
fn test_recursive_defs_are_left_alone() {
    let minified = minify(json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/Node" } },
        "$defs": {
            "Node": { "type": "object", "properties": { "child": { "$ref": "#/$defs/Node" } } }
        }
    }));

    assert!(minified["$defs"]["Node"].is_object());
}

/// D17 — a parameterless tool minifies to exactly this, and nothing more.
#[rstest]
fn test_parameterless_tool_schema() {
    let minified = minify(json!({ "type": "object", "properties": {} }));

    assert_eq!(
        serde_json::to_string(&minified).expect("a serialisable schema"),
        r#"{"additionalProperties":false,"type":"object"}"#
    );
}

/// An invented argument should be an error the model is told about in the schema, rather than
/// something it discovers at call time.
#[rstest]
fn test_additional_properties_is_closed_at_the_root() {
    let minified = minify(json!({ "type": "object", "properties": { "a": { "type": "string" } } }));

    assert_eq!(minified["additionalProperties"], json!(false));
}

/// Serialisation is byte-stable: `serde_json`'s map is a `BTreeMap`, so key order is the same
/// on every run and a golden file means something.
#[rstest]
fn test_serialisation_is_deterministic() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": { "z": { "type": "string" }, "a": { "type": "number" } }
    });

    let first = serde_json::to_string(&minify(schema.clone())).expect("serialisable");
    let second = serde_json::to_string(&minify(schema)).expect("serialisable");

    assert_eq!(first, second);
    assert_eq!(
        first,
        r#"{"additionalProperties":false,"properties":{"a":{"type":"number"},"z":{"type":"string"}},"type":"object"}"#
    );
}
