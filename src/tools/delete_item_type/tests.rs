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
    tools::delete_item_type::{DeleteItemType, DeleteItemTypeInput, MAX_KIND_BYTES},
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item_type_definition,
        mock_list_envelope,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    http::Method,
    matchers::{method, path, query_param},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The fixture definition's own path, which the delete targets.
const TYPE_PATH: &str = "/mia-platform.eu/v1/item-type-definitions/services.stable.example.com";

/// The family count of the fixture's served `v1`.
const V1_COUNT_PATH: &str = "/stable.example.com/v1/items/services/count";

/// The family count of a second served version.
const V2_COUNT_PATH: &str = "/stable.example.com/v2/items/services/count";

/// The global count, for versions no family route reaches.
const GLOBAL_COUNT_PATH: &str = "/items/count";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// The engine's cascade warning, verbatim (`apis/item_type_definitions/delete_by_name`).
const CASCADE_WARNING: &str = "An error occurred while cleaning up after deleting Item Type \
     Definition 'services.stable.example.com'. The system may still contain orphaned items, \
     relationships, or relationship constraints.";

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

/// Answers the `kind` lookup for `Service` with `definition`.
async fn mount_type(engine: &MockEngine, definition: Value) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![definition], None)),
        )
        .mount(engine.server())
        .await;
}

/// Answers a count on `url_path`.
async fn mount_count(engine: &MockEngine, url_path: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// A count of `n`.
fn mock_count(n: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "count": n }))
}

/// Answers the delete.
async fn mount_delete(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("DELETE"))
        .and(path(TYPE_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// A mock engine where `Service` (one served `v1`) has `items` items and the delete succeeds.
async fn mock_engine(items: u64) -> MockEngine {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(items)).await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    engine
}

/// Runs the tool and renders its answer exactly as the runtime does — engine warnings included.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    let context = mock_context_at(&engine.server().uri(), CALL_BUDGET);
    let input: DeleteItemTypeInput =
        serde_json::from_value(arguments).expect("the fixture arguments deserialise");

    let output = DeleteItemType.call(&context, input).await?;

    Ok(output.render(context.engine().call_warnings().collected().as_deref()))
}

/// Every request of `verb` on `url_path` the mock received, as its query string.
async fn requests(engine: &MockEngine, verb: Method, url_path: &str) -> Vec<String> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.method == verb && request.url.path() == url_path)
        .map(|request| request.url.query().unwrap_or_default().to_string())
        .collect()
}

/// Every `DELETE` the mock received.
async fn deletes(engine: &MockEngine) -> Vec<String> {
    requests(engine, Method::DELETE, TYPE_PATH).await
}

// ---------------------------------------------------------------------------------------------
// The two-phase refusal, first. No refusal path may issue a `DELETE`.
// ---------------------------------------------------------------------------------------------

/// Without `expected_items`, a type with items is refused — as an answer, with the count and the
/// instruction — and nothing is deleted.
#[rstest]
#[tokio::test]
async fn test_a_type_with_items_is_refused_the_first_time() {
    let engine = mock_engine(340).await;

    let answer = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect("a refusal is an answer, not an error");

    assert_eq!(
        serde_json::to_string(&answer).expect("serialisable"),
        json!({
            "deleted": false, "kind": "Service", "group": GROUP,
            "name": "services.stable.example.com", "itemsFound": 340,
            "action": "call again with expected_items: 340 to proceed", "warnings": []
        })
        .to_string()
    );
    assert!(
        deletes(&engine).await.is_empty(),
        "a refusal deletes nothing"
    );
}

/// The matching count proceeds, guarded by the type's `resourceVersion`.
#[rstest]
#[tokio::test]
async fn test_the_matching_count_proceeds() {
    let engine = mock_engine(340).await;

    let answer = run(&engine, json!({ "kind": "Service", "expected_items": 340 }))
        .await
        .expect("the delete succeeds");

    assert_eq!(
        serde_json::to_string(&answer).expect("serialisable"),
        json!({
            "deleted": true, "kind": "Service", "group": GROUP,
            "name": "services.stable.example.com", "itemsDeleted": 340,
            "relationshipsDeleted": null, "warnings": []
        })
        .to_string()
    );
    assert_eq!(deletes(&engine).await, vec!["resourceVersion=1"]);
}

/// A stale count is refused again with the **current** number — never "close enough".
#[rstest]
#[case::more_now(340, 341)]
#[case::fewer_now(340, 339)]
#[case::emptied_since(5, 0)]
#[tokio::test]
async fn test_a_stale_count_is_refused_with_the_current_one(
    #[case] expected: u64,
    #[case] now: u64,
) {
    let engine = mock_engine(now).await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "expected_items": expected }),
    )
    .await
    .expect("a refusal is an answer");

    assert_eq!(answer["deleted"], json!(false));
    assert_eq!(answer["itemsFound"], json!(now));
    assert_eq!(
        answer["action"],
        json!(format!("call again with expected_items: {now} to proceed"))
    );
    assert!(deletes(&engine).await.is_empty());
}

/// An empty type is deleted in one pass: the guard costs nothing in the safe case.
#[rstest]
#[tokio::test]
async fn test_an_empty_type_deletes_first_time() {
    let engine = mock_engine(0).await;

    let answer = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect("the delete succeeds");

    assert_eq!(answer["deleted"], json!(true));
    assert_eq!(answer["itemsDeleted"], json!(0));
    assert_eq!(deletes(&engine).await.len(), 1);
}

/// The count runs even when `expected_items` was given — what makes this a guard and not a
/// `confirm` flag.
#[rstest]
#[tokio::test]
async fn test_the_count_is_rechecked_when_a_number_is_given() {
    let engine = mock_engine(3).await;

    run(&engine, json!({ "kind": "Service", "expected_items": 3 }))
        .await
        .expect("the delete succeeds");

    assert_eq!(requests(&engine, Method::GET, V1_COUNT_PATH).await.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// The scope is every item under every declared version, and a count that cannot be made stops the
// delete.
// ---------------------------------------------------------------------------------------------

/// Items under every served version are summed; items under a version no longer served are
/// counted globally, since no family route reaches them.
#[rstest]
#[tokio::test]
async fn test_every_declared_version_is_counted() {
    let engine = MockEngine::start().await;
    let mut definition = mock_item_type_definition("Service", "services", GROUP);
    let v1 = definition["spec"]["versions"][0].clone();
    let mut v2 = v1.clone();
    v2["name"] = json!("v2");
    let mut v0 = v1.clone();
    v0["name"] = json!("v0");
    v0["served"] = json!(false);
    definition["spec"]["versions"] = json!([v0, v1, v2]);
    mount_type(&engine, definition).await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(10)).await;
    mount_count(&engine, V2_COUNT_PATH, mock_count(20)).await;
    mount_count(&engine, GLOBAL_COUNT_PATH, mock_count(3)).await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect("a refusal is an answer");

    assert_eq!(answer["itemsFound"], json!(33));

    let global = requests(&engine, Method::GET, GLOBAL_COUNT_PATH).await;
    assert_eq!(
        global.len(),
        1,
        "the unserved version is counted once, globally"
    );
    // base64url needs no percent-decoding, so the query string is split as it arrived.
    let rawq = global[0]
        .split('&')
        .find_map(|pair| pair.strip_prefix("rawq="))
        .expect("a rawq was sent");
    let decoded = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        String::from_utf8(URL_SAFE_NO_PAD.decode(rawq).expect("rawq is base64url"))
            .expect("rawq is UTF-8")
    };
    assert!(decoded.contains("stable.example.com/v0"), "{decoded}");
    assert!(!decoded.contains("stable.example.com/v1"), "{decoded}");
}

/// A count the caller may not make — or that fails — stops the delete: a guard that cannot count
/// must not pass.
#[rstest]
#[case::forbidden(403, codes::FORBIDDEN)]
#[case::unavailable(503, codes::CATALOG_UNAVAILABLE)]
#[tokio::test]
async fn test_a_failed_count_deletes_nothing(#[case] status: u16, #[case] code: &str) {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_count(&engine, V1_COUNT_PATH, mock_failure(status, "no")).await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let error = run(&engine, json!({ "kind": "Service", "expected_items": 0 }))
        .await
        .expect_err("an uncounted type is not deleted");

    assert_eq!(error.code, code);
    assert!(
        error.message.contains("nothing was deleted"),
        "{}",
        error.message
    );
    assert!(deletes(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The warning path, shared with `delete_item` and `apply_item_type`.
// ---------------------------------------------------------------------------------------------

/// A `204` with the cascade warning: the type is gone, and orphans may remain — both said.
#[rstest]
#[tokio::test]
async fn test_a_failed_cascade_is_reported_beside_the_delete() {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(0)).await;
    mount_delete(
        &engine,
        ResponseTemplate::new(204)
            .append_header("Warning", format!(r#"299 - "{CASCADE_WARNING}""#).as_str()),
    )
    .await;

    let answer = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["deleted"], json!(true));
    assert_eq!(answer["warnings"], json!([CASCADE_WARNING]));
}

// ---------------------------------------------------------------------------------------------
// The remaining error cases.
// ---------------------------------------------------------------------------------------------

/// A failure after the `DELETE` left may have landed; one on the count or the lookup did not.
#[rstest]
#[tokio::test]
async fn test_a_failed_delete_has_an_unknown_outcome() {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(0)).await;
    mount_delete(&engine, mock_failure(500, "Something went wrong")).await;

    let error = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect_err("a failed delete is an error");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNKNOWN_OUTCOME, Remedy::Unknown)
    );
}

/// A failed lookup deleted nothing, and is safe to retry.
#[rstest]
#[tokio::test]
async fn test_a_failed_lookup_is_catalog_unavailable() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(mock_failure(503, "unavailable"))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect_err("a failed lookup is an error");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
    assert!(deletes(&engine).await.is_empty());
}

/// A `409` is reported once, never retried.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_reported_not_retried() {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(0)).await;
    mount_delete(&engine, mock_failure(409, "Concurrent modification")).await;

    let error = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect_err("a conflict is an error");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CONFLICT, Remedy::RetryLater)
    );
    assert_eq!(deletes(&engine).await.len(), 1);
}

/// A `404` on the delete after the lookup found the type: someone else deleted it in between.
#[rstest]
#[tokio::test]
async fn test_a_concurrent_delete_is_not_reported_as_ours() {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(0)).await;
    mount_delete(&engine, mock_failure(404, "not found for deletion")).await;

    let error = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect_err("nothing was deleted by this call");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::Escalate)
    );
    assert!(
        error.message.contains("deleted by someone else"),
        "{}",
        error.message
    );
}

/// An unknown kind is `not_found` with near matches — **not** an idempotent success.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_offers_near_matches() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Servic"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition("Service", "services", GROUP)],
            None,
        )))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "kind": "Servic" }))
        .await
        .expect_err("an unknown kind is an error");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!(["Service"]))
    );
    assert!(deletes(&engine).await.is_empty());
}

/// A type read back without its `resourceVersion` is never deleted unguarded.
#[rstest]
#[tokio::test]
async fn test_a_type_without_a_resource_version_is_not_deleted() {
    let engine = MockEngine::start().await;
    let mut definition = mock_item_type_definition("Service", "services", GROUP);
    definition
        .as_object_mut()
        .map(|definition| definition.remove("resourceVersion"));
    mount_type(&engine, definition).await;
    mount_count(&engine, V1_COUNT_PATH, mock_count(0)).await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let error = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect_err("an unguarded delete is refused");

    assert_eq!(error.code, codes::SERVER_DEFECT);
    assert!(deletes(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Arguments checked before anything reaches the engine.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::empty_kind(json!({ "kind": "" }), "kind")]
#[case::long_kind(json!({ "kind": "A".repeat(MAX_KIND_BYTES + 1) }), "kind")]
#[case::malformed_kind(json!({ "kind": "my-kind" }), "kind")]
#[case::malformed_group(json!({ "kind": "Service", "group": "Not A Group" }), "group")]
#[tokio::test]
async fn test_a_malformed_argument_is_refused_before_the_engine(
    #[case] arguments: Value,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&engine, arguments)
        .await
        .expect_err("the argument is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(
        error.details.as_deref().map(|details| &details["field"]),
        Some(&json!(field))
    );
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

/// A negative count is not a number of items: refused by the runtime before the tool runs.
#[rstest]
fn test_a_negative_count_is_not_an_argument() {
    assert!(
        serde_json::from_value::<DeleteItemTypeInput>(
            json!({ "kind": "Service", "expected_items": -1 })
        )
        .is_err()
    );
}
