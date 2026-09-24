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
use crate::registry::contract::{ToolOutput, WARNINGS_KEY, invalid_arguments, success_result};
use catalog_client::{EngineWarning, error::codes};
use rstest::rstest;
use serde_json::json;

/// One engine warning, as the parser produces them.
fn mock_warning(text: &str) -> EngineWarning {
    EngineWarning {
        code: 299,
        text: text.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// §5.5 — `ToolOutput` is rendered as **one** JSON object, merged at the top level.
// ---------------------------------------------------------------------------------------------

/// A wrapper key would cost bytes on every response and the model would have to learn it, so
/// the payload **is** the object.
#[rstest]
fn test_a_payload_is_rendered_at_the_top_level() {
    let output = ToolOutput::new(json!({ "tenants": [], "current": "my-tenant" }));

    assert_eq!(
        output.render(None),
        json!({ "tenants": [], "current": "my-tenant" })
    );
}

/// D28 — a call that reached the engine always emits the key, so its absence is never
/// ambiguous.
#[rstest]
fn test_a_call_that_reached_the_engine_emits_the_key_even_when_empty() {
    let output = ToolOutput::new(json!({ "deleted": true }));

    assert_eq!(
        output.render(Some(&[])),
        json!({ "deleted": true, "warnings": [] })
    );
}

#[rstest]
fn test_engine_warnings_are_rendered_as_their_text() {
    let output = ToolOutput::new(json!({ "applied": true }));

    assert_eq!(
        output.render(Some(&[mock_warning("first"), mock_warning("second")])),
        json!({ "applied": true, "warnings": ["first", "second"] })
    );
}

/// D28 — a call that never reached the engine omits the key entirely rather than sending an
/// empty array the model has to interpret.
#[rstest]
fn test_a_call_that_never_reached_the_engine_omits_the_key() {
    let output = ToolOutput::new(json!({ "tenants": [] }));

    assert!(output.render(None).get(WARNINGS_KEY).is_none());
}

/// T3-D5 — a tool's own warning is rendered after the engine's, and brings the key with it even
/// on a call that never reached the engine.
#[rstest]
fn test_a_tools_own_warning_follows_the_engines() {
    let output = ToolOutput::new(json!({ "item": {} })).with_warning("relationships unavailable");

    assert_eq!(
        output.render(Some(&[mock_warning("type is deprecated")])),
        json!({ "item": {}, "warnings": ["type is deprecated", "relationships unavailable"] })
    );
    assert_eq!(
        output.render(None)[WARNINGS_KEY],
        json!(["relationships unavailable"])
    );
}

/// A tool that returns a bare value still gets an object: `warnings` has to have somewhere to
/// go, and the model has one shape to learn.
#[rstest]
fn test_a_bare_value_is_wrapped() {
    let output = ToolOutput::new(json!(["a", "b"]));

    assert_eq!(
        output.render(Some(&[])),
        json!({ "result": ["a", "b"], "warnings": [] })
    );
}

/// A tool **cannot** set `isError`: the runtime renders success, and returning `Err` is the only
/// way to fail (rule 2).
#[rstest]
fn test_a_successful_result_is_one_text_block_and_no_structured_content() {
    let result = success_result(&ToolOutput::new(json!({ "ok": true })), None);

    assert_eq!(result.content.len(), 1);
    assert_eq!(result.is_error, Some(false));
    assert!(
        result.structured_content.is_none(),
        "D15 — a result is never returned twice"
    );

    let serialised = serde_json::to_value(&result).expect("a serialisable result");
    assert_eq!(serialised["content"][0]["text"], json!(r#"{"ok":true}"#));
}

// ---------------------------------------------------------------------------------------------
// Rule 3 — an argument failure is a tool error naming its own field.
// ---------------------------------------------------------------------------------------------

/// The serde **path** is what makes the error actionable: "invalid type" alone tells a model
/// nothing about which argument to change.
#[rstest]
fn test_an_argument_failure_names_the_field() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Input {
        limit: u32,
    }

    let err = serde_path_to_error::deserialize::<_, Input>(json!({ "limit": "twelve" }))
        .expect_err("a wrong type is refused");

    let error = invalid_arguments(&err);

    assert_eq!(error.code, codes::INVALID_ARGUMENTS);
    assert_eq!(error.remedy, catalog_client::Remedy::RetryAfterChange);
    assert_eq!(
        error.details.expect("the field path is reported")["field"],
        json!("limit")
    );
}

/// A nested field reports its whole path, not just its leaf.
#[rstest]
fn test_a_nested_argument_failure_reports_its_path() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Inner {
        count: u32,
    }
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Input {
        filter: Inner,
    }

    let err =
        serde_path_to_error::deserialize::<_, Input>(json!({ "filter": { "count": "many" } }))
            .expect_err("a wrong type is refused");

    assert_eq!(
        invalid_arguments(&err).details.expect("a path")["field"],
        json!("filter.count")
    );
}
