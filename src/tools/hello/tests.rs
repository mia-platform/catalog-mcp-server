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
use crate::tools::hello::{TOOL_NAME, descriptor};
use rstest::rstest;

/// D16 — annotations are emitted only where they differ from the specification's defaults.
/// Omitting them entirely would declare this read-only probe destructive and open-world.
#[rstest]
fn test_annotations_declare_only_what_differs_from_the_defaults() {
    let annotations = descriptor().annotations;

    assert_eq!(annotations.read_only_hint, Some(true));
    assert_eq!(annotations.destructive_hint, None);
    assert_eq!(annotations.idempotent_hint, None);
    assert_eq!(annotations.open_world_hint, None);
    assert_eq!(annotations.title, None);
}

/// D17 — a parameterless tool minifies to exactly this, and nothing more.
#[rstest]
fn test_input_schema_is_the_parameterless_form() {
    let schema = serde_json::to_string(&descriptor().input_schema).expect("a serialisable schema");

    assert_eq!(schema, r#"{"additionalProperties":false,"type":"object"}"#);
}

#[rstest]
fn test_descriptor_names_the_tool() {
    assert_eq!(descriptor().name, TOOL_NAME);
    assert!(!descriptor().description.is_empty());
}
