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
    registry::contract::{CallContext, Tool},
    tools::get_item_schema::{GetItemSchema, GetItemSchemaInput, MAX_VERSION_BYTES},
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{MockEngine, mock_acl_context, mock_item_type_definition},
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// The recorded size of the realistic fixture's response (T6 §9). Regression detection on the
/// projection, not a limit.
const RECORDED_REALISTIC_BYTES: usize = 5_536;

/// How far that golden may drift.
const SIZE_TOLERANCE_PERCENT: usize = 1;

/// A `Skill` type shaped like the engine's: display names, a description, a briefing, a schema
/// with nesting, one selectable field, history on.
fn mock_skill_type() -> Value {
    let mut itd = mock_item_type_definition("Skill", "skills", "ai.mia-platform.eu");
    itd["metadata"]["description"] = json!("A specialized guideline to accomplish a task");
    itd["spec"]["names"]["displaySingular"] = json!("Skill");
    itd["spec"]["names"]["displayPlural"] = json!("Skills");
    itd["spec"]["llmDescription"] = json!("Use a skill when the task has a known procedure.");
    itd["spec"]["versions"][0]["schema"] = json!({ "openAPIV31Schema": mock_schema(3, 4) });
    itd["spec"]["versions"][0]["selectableFields"] = json!([{ "jsonPath": "spec.category" }]);
    itd
}

/// A JSON Schema `depth` levels deep and `width` properties wide at every level — large enough
/// that anything but the whole document is visibly not it.
fn mock_schema(depth: usize, width: usize) -> Value {
    if depth == 0 {
        return json!({ "type": "string", "description": "A leaf field." });
    }

    let properties: serde_json::Map<String, Value> = (0..width)
        .map(|index| (format!("field{index}"), mock_schema(depth - 1, width)))
        .collect();

    json!({ "type": "object", "description": "A nested object.", "properties": properties })
}

/// A `List` envelope.
fn mock_page(items: Vec<Value>) -> Value {
    json!({ "apiVersion": "v1", "kind": "List", "metadata": {}, "items": items })
}

/// A call context against `base_url`.
fn mock_context_at(base_url: &str, budget: Duration) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        Some("Bearer test-token"),
        None,
    ));
    let client = EngineClientFactory::new(
        base_url,
        "/",
        Duration::from_secs(5),
        Duration::from_millis(200),
        0,
    )
    .expect("the fixture URL is well formed")
    .bind(identity.clone(), Deadline::starting_now(budget));

    CallContext::new(
        client,
        Deadline::starting_now(budget),
        CancellationToken::new(),
        None,
        identity.tenant_key(),
    )
}

/// Serves `types` to the `kind` lookup for `kind`.
async fn mount_types(engine: &MockEngine, kind: &str, types: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param(
            "field",
            format!("spec.names.kind={kind}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(types)))
        .mount(engine.server())
        .await;
}

/// Runs the tool for `kind` and `version` against a lookup answering `types`.
async fn call_with(
    types: Vec<Value>,
    kind: &str,
    version: Option<&str>,
) -> Result<Value, ToolError> {
    let engine = MockEngine::start().await;
    mount_types(&engine, kind, types).await;

    run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        kind,
        version,
    )
    .await
}

/// Runs the tool in `context`.
async fn run(context: &CallContext, kind: &str, version: Option<&str>) -> Result<Value, ToolError> {
    GetItemSchema
        .call(
            context,
            GetItemSchemaInput {
                kind: kind.to_string(),
                group: None,
                version: version.map(str::to_string),
            },
        )
        .await
        .map(|output| output.payload().clone())
}

// ---------------------------------------------------------------------------------------------
// §4 — the projection.
// ---------------------------------------------------------------------------------------------

/// The documented fields, in the documented order, and nothing an agent cannot act on.
#[rstest]
#[tokio::test]
async fn test_the_output_is_the_documented_projection() {
    let payload = call_with(vec![mock_skill_type()], "Skill", None)
        .await
        .expect("described");

    let keys: Vec<&str> = payload
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "kind",
            "family",
            "group",
            "version",
            "displayName",
            "displayPlural",
            "description",
            "llmDescription",
            "schema",
            "selectableFields",
            "historyEnabled"
        ]
    );
    assert_eq!(payload["displayName"], json!("Skill"));
    assert_eq!(payload["selectableFields"], json!(["spec.category"]));
    assert_eq!(payload["historyEnabled"], json!(true));
    for dropped in [
        "uid",
        "urn",
        "resourceVersion",
        "scope",
        "served",
        "deprecated",
        "title",
    ] {
        assert!(
            !serde_json::to_string(&payload)
                .expect("serialises")
                .contains(&format!("\"{dropped}\"")),
            "`{dropped}` is not actionable and must not be returned"
        );
    }
}

/// **The schema is returned whole** — byte for byte the engine's, however large (§5, D34).
#[rstest]
#[tokio::test]
async fn test_the_schema_is_returned_whole() {
    let mut itd = mock_skill_type();
    let large = mock_schema(4, 6);
    itd["spec"]["versions"][0]["schema"] = json!({ "openAPIV31Schema": large.clone() });
    assert!(
        serde_json::to_string(&large).expect("serialises").len() > 20_000,
        "the fixture must be larger than the largest shipped type"
    );

    let payload = call_with(vec![itd], "Skill", None)
        .await
        .expect("described");

    assert_eq!(payload["schema"], large);
}

/// T6-D7 — the display name is `displaySingular`, never `metadata.title`, set or not.
#[rstest]
#[case::title_absent(None)]
#[case::title_set(Some("A title nobody uses"))]
#[tokio::test]
async fn test_the_display_name_never_comes_from_the_title(#[case] title: Option<&str>) {
    let mut itd = mock_skill_type();
    if let Some(title) = title {
        itd["metadata"]["title"] = json!(title);
    }

    let payload = call_with(vec![itd], "Skill", None)
        .await
        .expect("described");

    assert_eq!(payload["displayName"], json!("Skill"));
}

/// T6-D3 — the briefing is in full when present, and absent when absent or blank — never
/// synthesised from `description`.
#[rstest]
#[case::absent(None, None)]
#[case::blank(Some("   \n"), None)]
#[case::verbatim(
    Some("# Skills\n\nFirst.\n\n- one\n- two"),
    Some("# Skills\n\nFirst.\n\n- one\n- two")
)]
#[tokio::test]
async fn test_the_briefing_is_whole_or_absent(
    #[case] briefing: Option<&str>,
    #[case] expected: Option<&str>,
) {
    let mut itd = mock_skill_type();
    match briefing {
        Some(briefing) => itd["spec"]["llmDescription"] = json!(briefing),
        None => {
            itd["spec"]
                .as_object_mut()
                .expect("spec")
                .remove("llmDescription");
        }
    }

    let payload = call_with(vec![itd], "Skill", None)
        .await
        .expect("described");

    assert_eq!(
        payload.get("llmDescription").and_then(Value::as_str),
        expected
    );
}

/// `selectableFields` is flattened to paths, and omitted — not empty — when there are none.
#[rstest]
#[tokio::test]
async fn test_selectable_fields_are_omitted_when_there_are_none() {
    let mut itd = mock_skill_type();
    itd["spec"]["versions"][0]
        .as_object_mut()
        .expect("a version")
        .remove("selectableFields");

    let payload = call_with(vec![itd], "Skill", None)
        .await
        .expect("described");

    assert!(payload.get("selectableFields").is_none(), "{payload}");
}

// ---------------------------------------------------------------------------------------------
// T6-D8 — versions.
// ---------------------------------------------------------------------------------------------

/// A type with two served versions, the older deprecated.
fn mock_two_versions() -> Value {
    let mut itd = mock_skill_type();
    let mut v2 = itd["spec"]["versions"][0].clone();
    v2["name"] = json!("v2");
    v2["schema"] = json!({ "openAPIV31Schema": { "type": "object", "description": "v2" } });
    itd["spec"]["versions"][0]["deprecated"] = json!(true);
    itd["spec"]["versions"]
        .as_array_mut()
        .expect("versions")
        .push(v2);
    itd
}

/// Absent `version`: the core's rule picks — a non-deprecated one wins.
#[rstest]
#[tokio::test]
async fn test_the_served_version_is_selected_by_the_core_rule() {
    let payload = call_with(vec![mock_two_versions()], "Skill", None)
        .await
        .expect("described");

    assert_eq!(payload["version"], json!("v2"));
    assert_eq!(payload["schema"]["description"], json!("v2"));
}

/// An explicit served version is honoured, even the deprecated one.
#[rstest]
#[tokio::test]
async fn test_an_explicit_served_version_is_honoured() {
    let payload = call_with(vec![mock_two_versions()], "Skill", Some("v1"))
        .await
        .expect("described");

    assert_eq!(payload["version"], json!("v1"));
}

/// An unknown or unserved version is an error naming the served ones — never substituted.
#[rstest]
#[case::unknown("v9")]
#[case::unserved("v3")]
#[tokio::test]
async fn test_a_version_that_is_not_served_is_refused_naming_the_served(#[case] version: &str) {
    let mut itd = mock_two_versions();
    let mut v3 = itd["spec"]["versions"][0].clone();
    v3["name"] = json!("v3");
    v3["served"] = json!(false);
    itd["spec"]["versions"]
        .as_array_mut()
        .expect("versions")
        .push(v3);

    let error = call_with(vec![itd], "Skill", Some(version))
        .await
        .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["servedVersions"].clone()),
        Some(json!(["v1", "v2"]))
    );
}

/// A type with no served version is `unaddressable_type` — with or without an explicit version.
#[rstest]
#[case::no_version(None)]
#[case::explicit(Some("v1"))]
#[tokio::test]
async fn test_a_type_with_nothing_served_is_unaddressable(#[case] version: Option<&str>) {
    let mut itd = mock_skill_type();
    itd["spec"]["versions"][0]["served"] = json!(false);

    let error = call_with(vec![itd], "Skill", version)
        .await
        .expect_err("unaddressable");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNADDRESSABLE_TYPE, Remedy::Escalate)
    );
}

// ---------------------------------------------------------------------------------------------
// §6 — the rest of the table.
// ---------------------------------------------------------------------------------------------

/// An unknown `kind` returns candidates, not a bare `404`.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_returns_candidates() {
    let engine = MockEngine::start().await;
    mount_types(&engine, "Skil", vec![]).await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param_is_missing("field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_skill_type()])))
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        "Skil",
        None,
    )
    .await
    .expect_err("an unknown kind fails");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!(["Skill"]))
    );
    assert!(
        error
            .next_step
            .as_deref()
            .is_some_and(|step| step.contains("list_catalog_types"))
    );
}

/// DR-80 — a kind shared by two groups is answered with the candidates; neither is picked.
#[rstest]
#[tokio::test]
async fn test_a_shared_kind_returns_the_candidates() {
    let mut other = mock_skill_type();
    other["spec"]["group"] = json!("example.com");
    other["metadata"]["name"] = json!("skills.example.com");

    let error = call_with(vec![mock_skill_type(), other], "Skill", None)
        .await
        .expect_err("never guessed");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!([
            { "kind": "Skill", "group": "ai.mia-platform.eu", "family": "skills" },
            { "kind": "Skill", "group": "example.com", "family": "skills" }
        ]))
    );
}

/// With `group` the lookup is exact; two rows for one `(group, kind)` are then a
/// `server_defect` (T6-D2), because the engine's own constraint forbids them.
#[rstest]
#[tokio::test]
async fn test_a_group_makes_the_lookup_exact() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.group=ai.mia-platform.eu"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_skill_type()])))
        .mount(engine.server())
        .await;
    let context = mock_context_at(&engine.server().uri(), CALL_BUDGET);

    let payload = GetItemSchema
        .call(
            &context,
            GetItemSchemaInput {
                kind: "Skill".to_string(),
                group: Some("ai.mia-platform.eu".to_string()),
                version: None,
            },
        )
        .await
        .expect("the pair names one type")
        .payload()
        .clone();

    assert_eq!(payload["group"], json!("ai.mia-platform.eu"));
    let query = engine
        .server()
        .received_requests()
        .await
        .unwrap_or_default()[0]
        .url
        .query()
        .unwrap_or_default()
        .to_string();
    assert!(query.contains("limit=2"), "{query}");
}

#[rstest]
#[tokio::test]
async fn test_two_types_for_one_group_and_kind_are_a_server_defect() {
    let engine = MockEngine::start().await;
    let mut duplicate = mock_skill_type();
    duplicate["metadata"]["name"] = json!("skills.ai.mia-platform.eu-again");
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.group=ai.mia-platform.eu"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_skill_type(), duplicate])),
        )
        .mount(engine.server())
        .await;
    let context = mock_context_at(&engine.server().uri(), CALL_BUDGET);

    let error = GetItemSchema
        .call(
            &context,
            GetItemSchemaInput {
                kind: "Skill".to_string(),
                group: Some("ai.mia-platform.eu".to_string()),
                version: None,
            },
        )
        .await
        .expect_err("an impossible pair is never guessed");

    assert_eq!(
        (error.code, error.remedy),
        (codes::SERVER_DEFECT, Remedy::Escalate)
    );
}

/// Engine unavailability, and the call's own deadline.
#[rstest]
#[case::server_error(500, codes::CATALOG_UNAVAILABLE)]
#[case::unavailable(503, codes::CATALOG_UNAVAILABLE)]
#[tokio::test]
async fn test_an_engine_failure_is_catalog_unavailable(#[case] status: u16, #[case] code: &str) {
    let engine = MockEngine::start().await;
    engine.get_error(TYPES_PATH, status, "boom").await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        "Skill",
        None,
    )
    .await
    .expect_err("fails");

    assert_eq!((error.code, error.remedy), (code, Remedy::Retry));
}

#[rstest]
#[tokio::test]
async fn test_a_spent_deadline_is_deadline_exceeded() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_page(vec![mock_skill_type()]))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), Duration::from_millis(100)),
        "Skill",
        None,
    )
    .await
    .expect_err("the deadline runs out first");

    assert_eq!(
        (error.code, error.remedy),
        (codes::DEADLINE_EXCEEDED, Remedy::Retry)
    );
}

/// Boundary bounds are `invalid_input` naming the parameter, before the engine is asked.
#[rstest]
#[case::empty_kind(String::new(), None, "kind")]
#[case::bad_kind("not a kind".to_string(), None, "kind")]
#[case::long_kind("K".repeat(129), None, "kind")]
#[case::empty_version("Skill".to_string(), Some(String::new()), "version")]
#[case::long_version("Skill".to_string(), Some("v".repeat(MAX_VERSION_BYTES + 1)), "version")]
#[tokio::test]
async fn test_an_input_over_its_bound_is_refused(
    #[case] kind: String,
    #[case] version: Option<String>,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        &kind,
        version.as_deref(),
    )
    .await
    .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["field"].clone()),
        Some(json!(field))
    );
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "refused before the engine"
    );
}

/// T6-D1 — the happy path is **one** engine call, the core's lookup, asking for two rows.
#[rstest]
#[tokio::test]
async fn test_the_happy_path_is_one_request_asking_for_two_rows() {
    let engine = MockEngine::start().await;
    mount_types(&engine, "Skill", vec![mock_skill_type()]).await;

    run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        "Skill",
        None,
    )
    .await
    .expect("described");

    let requests = engine
        .server()
        .received_requests()
        .await
        .unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .url
            .query()
            .is_some_and(|query| query.contains("limit=2"))
    );
}

// ---------------------------------------------------------------------------------------------
// §9 — the byte golden.
// ---------------------------------------------------------------------------------------------

/// The realistic fixture serialises to a recorded size — a projection regression is a diff.
#[rstest]
#[tokio::test]
async fn test_the_realistic_type_serialises_to_its_recorded_size() {
    let payload = call_with(vec![mock_skill_type()], "Skill", None)
        .await
        .expect("described");
    let actual = serde_json::to_string(&payload).expect("serialises").len();

    let tolerance = RECORDED_REALISTIC_BYTES * SIZE_TOLERANCE_PERCENT / 100;
    assert!(
        actual.abs_diff(RECORDED_REALISTIC_BYTES) <= tolerance,
        "the realistic type serialises to {actual} B, recorded {RECORDED_REALISTIC_BYTES} B \
         (±{SIZE_TOLERANCE_PERCENT} %). Update the recording if the growth is intended."
    );
}
