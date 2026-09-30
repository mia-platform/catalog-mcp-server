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
    tools::apply_item::{
        ApplyItem, ApplyItemInput, MAX_KIND_BYTES, MAX_NAME_BYTES, dotted, schema_field,
        violation_paths,
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MOCK_ITEM_NAME, MockEngine, mock_acl_context, mock_error_body, mock_item,
        mock_item_type_definition,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    http::Method,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// Where the fixture item lives: `mock_item`'s group, version and family.
const ITEM_PATH: &str = "/stable.example.com/v1/items/services/example-item";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// The engine's schema-violation message, verbatim in shape (`models/lib/json_schema.rs`).
const SCHEMA_VIOLATION: &str = "Body does not conform to Item Type Definition schema: path \
     \"/spec/replicas\": \"two\" is not of type \"integer\"; path \"/spec/steps/0\": \"title\" is \
     a required property";

/// An item carrying **all six** of the metadata fields a patch-only `PUT` would wipe.
fn mock_rich_item() -> Value {
    let mut item = mock_item(MOCK_ITEM_NAME);
    item["metadata"]["annotations"] = json!({ "example.com/note": "kept" });
    item["metadata"]["links"] = json!([{ "title": "Docs", "url": "https://example.com/docs" }]);
    item["spec"] = json!({ "replicas": 2, "owner": { "team": "platform" } });
    item
}

/// A list envelope around `items`.
fn mock_page(items: Vec<Value>) -> Value {
    json!({ "apiVersion": "v1", "kind": "List", "metadata": {}, "items": items })
}

/// A call context against `base_url`, with no retries so each mock answers once.
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

/// An input, deserialised exactly as the runtime deserialises a call's arguments — which is what
/// keeps an explicit `null` a deletion.
fn mock_input(arguments: Value) -> ApplyItemInput {
    serde_json::from_value(arguments).expect("the fixture arguments deserialise")
}

/// A mock engine that knows the `Service` type.
async fn mock_engine() -> MockEngine {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![
            mock_item_type_definition("Service", "services", GROUP),
        ])))
        .mount(engine.server())
        .await;

    engine
}

/// Answers the pre-read.
async fn mount_read(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(ITEM_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Answers the write.
async fn mount_write(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("PUT"))
        .and(path(ITEM_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// Runs the tool against `engine`.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    let context = mock_context_at(&engine.server().uri(), CALL_BUDGET);

    ApplyItem
        .call(&context, mock_input(arguments))
        .await
        .map(|output| output.payload().clone())
}

/// Every request of `verb` the mock received on the item, in order.
async fn requests_on_item(engine: &MockEngine, verb: Method) -> Vec<Value> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.method == verb && request.url.path() == ITEM_PATH)
        .map(|request| serde_json::from_slice(&request.body).unwrap_or(Value::Null))
        .collect()
}

/// The one `PUT` body sent.
async fn sent_body(engine: &MockEngine) -> Value {
    let mut bodies = requests_on_item(engine, Method::PUT).await;
    assert_eq!(bodies.len(), 1, "exactly one write");

    bodies.remove(0)
}

/// A read and a write that both succeed, the write answering `written`.
async fn mock_update(written: Value) -> MockEngine {
    let engine = mock_engine().await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_rich_item()),
    )
    .await;
    mount_write(&engine, ResponseTemplate::new(200).set_body_json(written)).await;

    engine
}

// ---------------------------------------------------------------------------------------------
// T8-D1 — the wipe regression, first. If this ever fails, someone "simplified" the read away.
// ---------------------------------------------------------------------------------------------

/// A patch naming one spec field leaves **all six** metadata fields where they were. A `PUT` is a
/// full replace, so a body built from the patch alone would have written each of them as absent.
#[rstest]
#[tokio::test]
async fn test_a_spec_patch_preserves_every_metadata_field() {
    let engine = mock_update(mock_rich_item()).await;

    run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "x": 1 } }),
    )
    .await
    .expect("the write succeeds");

    let sent = sent_body(&engine).await;
    let before = mock_rich_item();
    for field in [
        "title",
        "description",
        "labels",
        "tags",
        "annotations",
        "links",
    ] {
        assert_eq!(
            sent["metadata"][field], before["metadata"][field],
            "`metadata.{field}` must survive a spec-only patch"
        );
    }
    assert_eq!(
        sent["spec"],
        json!({ "replicas": 2, "owner": { "team": "platform" }, "x": 1 })
    );
}

// ---------------------------------------------------------------------------------------------
// T8-D2 — how the merge is used: null deletes, lists replace, `{}` is nothing.
// ---------------------------------------------------------------------------------------------

/// `null` removes one label and leaves the others.
#[rstest]
#[tokio::test]
async fn test_null_deletes_one_label() {
    let engine = mock_update(mock_rich_item()).await;

    run(
        &engine,
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service",
            "metadata": { "labels": { "environment": null, "tier": "backend" } }
        }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(
        sent_body(&engine).await["metadata"]["labels"],
        json!({ "tier": "backend" })
    );
}

/// An explicit `null` reaches the merge as a deletion; serde's own `Option` handling would have
/// turned it into "not mentioned" and silently kept the title (DR-92).
#[rstest]
#[tokio::test]
async fn test_null_deletes_the_title() {
    let engine = mock_update(mock_rich_item()).await;

    run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "metadata": { "title": null } }),
    )
    .await
    .expect("the write succeeds");

    let sent = sent_body(&engine).await;
    assert!(sent["metadata"].get("title").is_none(), "{sent}");
    assert_eq!(
        sent["metadata"]["description"],
        json!("A service used in tests.")
    );
}

/// A list is replaced, never appended to.
#[rstest]
#[tokio::test]
async fn test_tags_are_replaced_not_appended() {
    let engine = mock_update(mock_rich_item()).await;

    run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "metadata": { "tags": ["grpc"] } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(
        sent_body(&engine).await["metadata"]["tags"],
        json!(["grpc"])
    );
}

/// `spec: {}` is an empty patch, **not** "empty the spec".
#[rstest]
#[tokio::test]
async fn test_an_empty_spec_is_an_empty_patch() {
    let engine = mock_update(mock_rich_item()).await;

    run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": {} }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(sent_body(&engine).await["spec"], mock_rich_item()["spec"]);
}

/// Removing spec fields takes explicit `null`s, field by field.
#[rstest]
#[tokio::test]
async fn test_a_literal_wipe_needs_explicit_nulls() {
    let engine = mock_update(mock_rich_item()).await;

    run(
        &engine,
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service",
            "spec": { "replicas": null, "owner": null }
        }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(sent_body(&engine).await["spec"], json!({}));
}

// ---------------------------------------------------------------------------------------------
// T8-D4 — `customFields`.
// ---------------------------------------------------------------------------------------------

/// Top-level `customFields` is not an argument at all, and is refused by name before the tool
/// runs; so is one inside `metadata`.
#[rstest]
#[case::top_level(json!({ "name": "example-item", "kind": "Service", "customFields": {} }))]
#[case::in_metadata(
    json!({ "name": "example-item", "kind": "Service", "metadata": { "customFields": {} } })
)]
fn test_custom_fields_are_not_an_argument(#[case] arguments: Value) {
    let error =
        serde_json::from_value::<ApplyItemInput>(arguments).expect_err("customFields is refused");

    assert!(error.to_string().contains("`customFields`"), "{error}");
}

/// `customFields` inside `spec` is refused with the reason, and nothing is written.
#[rstest]
#[tokio::test]
async fn test_custom_fields_inside_spec_are_refused() {
    let engine = mock_update(mock_rich_item()).await;

    let error = run(
        &engine,
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service",
            "spec": { "customFields": { "cost-center": "42" } }
        }),
    )
    .await
    .expect_err("customFields is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert!(error.message.contains("Custom fields"), "{}", error.message);
    assert_eq!(
        error.details.as_deref(),
        Some(&json!({ "field": "spec.customFields" }))
    );
    assert!(requests_on_item(&engine, Method::PUT).await.is_empty());
}

/// Custom fields the pre-read returned are **not** echoed into the write, where the engine would
/// ignore them.
#[rstest]
#[tokio::test]
async fn test_custom_fields_from_the_pre_read_are_stripped() {
    let engine = mock_engine().await;
    let mut stored = mock_rich_item();
    stored["customFields"] = json!({ "cost-center": "42" });
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(stored.clone()),
    )
    .await;
    mount_write(&engine, ResponseTemplate::new(200).set_body_json(stored)).await;

    run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 3 } }),
    )
    .await
    .expect("the write succeeds");

    assert!(sent_body(&engine).await.get("customFields").is_none());
}

// ---------------------------------------------------------------------------------------------
// T8-D3 — `409`: once is resolved by re-reading, twice is reported.
// ---------------------------------------------------------------------------------------------

/// One conflict resolves with `retried: true`, and the second attempt carries the
/// `resourceVersion` of a **fresh** read — reusing the stale one would clobber the other writer.
#[rstest]
#[tokio::test]
async fn test_one_conflict_is_resolved_by_re_reading() {
    let engine = mock_engine().await;

    Mock::given(method("GET"))
        .and(path(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_rich_item()))
        .up_to_n_times(1)
        .mount(engine.server())
        .await;
    let mut moved = mock_rich_item();
    moved["resourceVersion"] = json!("7");
    moved["metadata"]["labels"]["tier"] = json!("backend");
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(moved.clone()),
    )
    .await;

    Mock::given(method("PUT"))
        .and(path(ITEM_PATH))
        .respond_with(mock_failure(409, "Concurrent modification"))
        .up_to_n_times(1)
        .mount(engine.server())
        .await;
    let mut written = moved.clone();
    written["resourceVersion"] = json!("8");
    written["spec"]["replicas"] = json!(3);
    mount_write(&engine, ResponseTemplate::new(200).set_body_json(written)).await;

    let payload = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 3 } }),
    )
    .await
    .expect("the retried write succeeds");

    assert_eq!(payload["retried"], json!(true));
    assert_eq!(payload["changed"], json!(["spec.replicas"]));

    assert_eq!(
        requests_on_item(&engine, Method::GET).await.len(),
        2,
        "re-read"
    );
    let writes = requests_on_item(&engine, Method::PUT).await;
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0]["resourceVersion"], json!("1"));
    assert_eq!(writes[1]["resourceVersion"], json!("7"), "the fresh token");
    assert_eq!(
        writes[1]["metadata"]["labels"]["tier"],
        json!("backend"),
        "the other writer's change is kept"
    );
}

/// A second conflict is real contention: reported, not retried again.
#[rstest]
#[tokio::test]
async fn test_two_conflicts_are_reported() {
    let engine = mock_engine().await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_rich_item()),
    )
    .await;
    mount_write(&engine, mock_failure(409, "Concurrent modification")).await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 3 } }),
    )
    .await
    .expect_err("two conflicts are an error");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, Remedy::RetryLater);
    assert_eq!(requests_on_item(&engine, Method::PUT).await.len(), 2);
}

// ---------------------------------------------------------------------------------------------
// The create path, and what the answer says.
// ---------------------------------------------------------------------------------------------

/// A `404` on the read is a create: no `resourceVersion`, the identity fields the engine requires,
/// and `created: true`.
#[rstest]
#[tokio::test]
async fn test_a_missing_item_is_created() {
    let engine = mock_engine().await;
    mount_read(&engine, mock_failure(404, "not found")).await;
    mount_write(
        &engine,
        ResponseTemplate::new(201).set_body_json(mock_item(MOCK_ITEM_NAME)),
    )
    .await;

    let payload = run(
        &engine,
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service",
            "spec": { "replicas": 2 }, "metadata": { "title": "Example Service", "tags": null }
        }),
    )
    .await
    .expect("the create succeeds");

    assert_eq!(payload["created"], json!(true));

    let sent = sent_body(&engine).await;
    assert!(sent.get("resourceVersion").is_none());
    assert_eq!(
        sent,
        json!({
            "apiVersion": "stable.example.com/v1",
            "kind": "Service",
            "metadata": { "title": "Example Service", "name": MOCK_ITEM_NAME },
            "spec": { "replicas": 2 }
        })
    );
}

/// The answer is what the write did, in the documented order — not the item.
#[rstest]
#[tokio::test]
async fn test_the_answer_reports_the_change() {
    let mut written = mock_rich_item();
    written["spec"]["owner"]["team"] = json!("catalog");
    written["resourceVersion"] = json!("2");
    written["metadata"]["updateTimestamp"] = json!("2026-09-30T08:00:00Z");
    let engine = mock_update(written).await;

    let payload = run(
        &engine,
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service",
            "spec": { "owner": { "team": "catalog" } }
        }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(
        serde_json::to_string(&payload).expect("serialisable"),
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service", "group": GROUP, "version": "v1",
            "family": "services", "created": false, "changed": ["spec.owner.team"],
            "retried": false
        })
        .to_string()
    );
}

/// A patch that changes nothing says so: `changed` is present and empty, even though the engine
/// moved its own `resourceVersion` and timestamp.
#[rstest]
#[tokio::test]
async fn test_a_no_op_reports_nothing_changed() {
    let mut written = mock_rich_item();
    written["resourceVersion"] = json!("2");
    written["metadata"]["updateTimestamp"] = json!("2026-09-30T08:00:00Z");
    let engine = mock_update(written).await;

    let payload = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 2 } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(payload["changed"], json!([]));
    assert_eq!(payload["created"], json!(false));
}

// ---------------------------------------------------------------------------------------------
// §6 — every row, `code` and `remedy`.
// ---------------------------------------------------------------------------------------------

/// A schema violation names the offending field in `details.path` and points at the rules of
/// **just** the fields that failed (DR-86).
#[rstest]
#[tokio::test]
async fn test_a_schema_violation_names_the_field_and_its_rules() {
    let engine = mock_engine().await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_rich_item()),
    )
    .await;
    mount_write(&engine, mock_failure(400, SCHEMA_VIOLATION)).await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": "two" } }),
    )
    .await
    .expect_err("the engine rejects the item");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error.message, SCHEMA_VIOLATION,
        "the engine's reasons, whole"
    );
    assert_eq!(
        error.details.as_deref().map(|details| &details["path"]),
        Some(&json!("spec.replicas"))
    );

    let next_step = error.next_step.expect("a next step");
    assert!(next_step.contains("get_item_schema"), "{next_step}");
    assert!(
        next_step.contains(r#""fields":["spec.replicas","spec.steps"]"#),
        "{next_step}"
    );
    assert!(
        next_step.contains(r#""group":"stable.example.com""#),
        "{next_step}"
    );
}

/// A `400` that names no location is relayed untouched: nothing is claimed about it.
#[rstest]
#[tokio::test]
async fn test_a_bad_request_without_a_location_is_relayed() {
    let engine = mock_engine().await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_rich_item()),
    )
    .await;
    mount_write(&engine, mock_failure(400, "spec is required")).await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 3 } }),
    )
    .await
    .expect_err("the engine rejects the item");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert!(error.next_step.is_none());
}

/// An unknown `kind` comes back with near matches — **never** a guessed type for a write.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_returns_candidates() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Servic"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![])))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param_is_missing("field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![
            mock_item_type_definition("Service", "services", GROUP),
        ])))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "name": MOCK_ITEM_NAME, "kind": "Servic" }))
        .await
        .expect_err("an unknown kind is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!(["Service"]))
    );
    assert!(requests_on_item(&engine, Method::PUT).await.is_empty());
}

/// A type with no served version cannot be addressed, so nothing is read or written (D30).
#[rstest]
#[tokio::test]
async fn test_a_type_with_no_served_version_is_unaddressable() {
    let engine = MockEngine::start().await;
    let mut unserved = mock_item_type_definition("Service", "services", GROUP);
    unserved["spec"]["versions"][0]["served"] = json!(false);
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![unserved])))
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service" }),
    )
    .await
    .expect_err("an unserved type is an error");

    assert_eq!(error.code, codes::UNADDRESSABLE_TYPE);
    assert_eq!(error.remedy, Remedy::Escalate);
}

/// A failure **after** the `PUT` left may have committed: `unknown_outcome`, never a clean
/// failure (D20).
#[rstest]
#[case::internal_error(500)]
#[case::bad_gateway(502)]
#[case::unavailable(503)]
#[tokio::test]
async fn test_a_failed_write_has_an_unknown_outcome(#[case] status: u16) {
    let engine = mock_engine().await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_rich_item()),
    )
    .await;
    mount_write(&engine, mock_failure(status, "Something went wrong")).await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 3 } }),
    )
    .await
    .expect_err("a failed write is an error");

    assert_eq!(error.code, codes::UNKNOWN_OUTCOME);
    assert_eq!(error.remedy, Remedy::Unknown);
}

/// A failure on the **pre-read** wrote nothing, and is safe to retry.
#[rstest]
#[case::internal_error(500)]
#[case::unavailable(503)]
#[tokio::test]
async fn test_a_failed_pre_read_is_catalog_unavailable(#[case] status: u16) {
    let engine = mock_engine().await;
    mount_read(&engine, mock_failure(status, "Something went wrong")).await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "spec": { "replicas": 3 } }),
    )
    .await
    .expect_err("a failed read is an error");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert_eq!(error.remedy, Remedy::Retry);
    assert!(requests_on_item(&engine, Method::PUT).await.is_empty());
}

/// A call with no time left fails before anything is dispatched.
#[rstest]
#[tokio::test]
async fn test_an_exhausted_deadline_dispatches_nothing() {
    let engine = mock_engine().await;
    let context = mock_context_at(&engine.server().uri(), Duration::ZERO);

    let error = ApplyItem
        .call(
            &context,
            mock_input(json!({ "name": MOCK_ITEM_NAME, "kind": "Service" })),
        )
        .await
        .expect_err("no time left is an error");

    assert_eq!(error.code, codes::DEADLINE_EXCEEDED);
    assert_eq!(error.remedy, Remedy::Retry);
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------------------------
// NFR-10 — arguments checked before anything reaches the engine.
// ---------------------------------------------------------------------------------------------

/// A malformed argument is refused naming itself, and costs no engine call.
#[rstest]
#[case::empty_name(json!({ "name": "", "kind": "Service" }), "name")]
#[case::long_name(json!({ "name": "a".repeat(MAX_NAME_BYTES + 1), "kind": "Service" }), "name")]
#[case::uppercase_name(json!({ "name": "Example-Item", "kind": "Service" }), "name")]
#[case::empty_kind(json!({ "name": "example-item", "kind": "" }), "kind")]
#[case::long_kind(json!({ "name": "example-item", "kind": "A".repeat(MAX_KIND_BYTES + 1) }), "kind")]
#[case::malformed_kind(json!({ "name": "example-item", "kind": "my-kind" }), "kind")]
#[case::malformed_group(
    json!({ "name": "example-item", "kind": "Service", "group": "Not A Group" }),
    "group"
)]
#[case::spec_not_an_object(json!({ "name": "example-item", "kind": "Service", "spec": [1] }), "spec")]
#[case::spec_null(json!({ "name": "example-item", "kind": "Service", "spec": null }), "spec")]
#[tokio::test]
async fn test_a_malformed_argument_is_refused(#[case] arguments: Value, #[case] field: &str) {
    let engine = MockEngine::start().await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_rich_item()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![
            mock_item_type_definition("Service", "services", GROUP),
        ])))
        .mount(engine.server())
        .await;

    let error = run(&engine, arguments)
        .await
        .expect_err("the argument is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error.details.as_deref().map(|details| &details["field"]),
        Some(&json!(field))
    );
    assert!(requests_on_item(&engine, Method::PUT).await.is_empty());
}

/// `null` and absence stay distinct through deserialisation, on every metadata field.
#[rstest]
fn test_null_and_absent_stay_distinct() {
    let input = mock_input(json!({
        "name": "example-item", "kind": "Service", "spec": { "a": null },
        "metadata": {
            "title": null, "description": null, "labels": null,
            "tags": null, "annotations": null, "links": null
        }
    }));
    let metadata = input.metadata.expect("metadata is present");

    for field in [
        metadata.title,
        metadata.description,
        metadata.labels,
        metadata.tags,
        metadata.annotations,
        metadata.links,
    ] {
        assert_eq!(field, Some(Value::Null));
    }
    assert_eq!(input.spec, Some(json!({ "a": null })));

    let absent = mock_input(json!({ "name": "example-item", "kind": "Service", "metadata": {} }));
    assert_eq!(absent.spec, None);
    assert_eq!(absent.metadata.and_then(|metadata| metadata.title), None);
}

// ---------------------------------------------------------------------------------------------
// Reading the engine's schema-violation message.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_violation_paths_are_read_in_order() {
    assert_eq!(
        violation_paths(SCHEMA_VIOLATION),
        vec!["spec.replicas", "spec.steps.0"]
    );
}

#[rstest]
fn test_the_document_root_is_not_a_field() {
    assert!(
        violation_paths(
            "Body does not conform to Item Type Definition schema: path \"/\": \"spec\" is a \
             required property"
        )
        .is_empty()
    );
}

#[rstest]
#[case::plain("/spec/owner/team", "spec.owner.team")]
#[case::escaped(
    "/metadata/annotations/example.com~1note",
    "metadata.annotations.example.com/note"
)]
#[case::tilde("/spec/a~0b", "spec.a~b")]
#[case::root("/", "")]
fn test_a_pointer_becomes_a_dotted_path(#[case] pointer: &str, #[case] expected: &str) {
    assert_eq!(dotted(pointer), expected);
}

#[rstest]
#[case::index("spec.steps.0.title", "spec.steps.title")]
#[case::no_index("spec.owner.team", "spec.owner.team")]
fn test_array_indices_are_dropped_for_the_schema(#[case] path: &str, #[case] expected: &str) {
    assert_eq!(schema_field(path), expected);
}
