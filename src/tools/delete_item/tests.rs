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
    tools::delete_item::{
        DeleteItem, DeleteItemInput, MAX_KIND_BYTES, MAX_NAME_BYTES, RELATIONSHIP_COUNT_PAGE,
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MOCK_ITEM_NAME, MockEngine, mock_acl_context, mock_error_body, mock_item,
        mock_item_type_definition, mock_list_envelope,
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

/// Where the fixture item lives: `mock_item`'s group, version and family.
const ITEM_PATH: &str = "/stable.example.com/v1/items/services/example-item";

/// The fixture item's relationships.
const RELATIONSHIPS_PATH: &str =
    "/bff/stable.example.com/v1/items/services/example-item/relationships";

/// The global listing the kindless probe reads.
const ITEMS_PATH: &str = "/items";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// The engine's cascade warning, verbatim (`apis/items/delete_by_name`).
const CASCADE_WARNING: &str = "An error occurred while cleaning up after deleting item \
     'example-item'. The system may still contain orphaned relationships.";

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

/// One relationship entry touching the fixture item.
fn mock_entry(direction: &str, index: usize) -> Value {
    let me = format!("urn:mia-platform-catalog:{GROUP}:v1:Service:{MOCK_ITEM_NAME}");
    let other = format!("urn:mia-platform-catalog:{GROUP}:v1:Service:other-{index}");
    let (source, target) = match direction {
        "outbound" => (me, other),
        _ => (other, me),
    };

    json!({
        "direction": direction,
        "relationship": {
            "apiVersion": "mia-platform.eu/v1",
            "kind": "Relationship",
            "metadata": { "name": format!("relationship-{direction}-{index}"), "family": "relationships" },
            "spec": {
                "sourceRef": source,
                "targetRef": target,
                "typeRef": "urn:mia-platform-catalog:mia-platform.eu:v1:RelationshipType:depends-on"
            },
            "resourceVersion": "1"
        }
    })
}

/// A relationships page, with a continuation when `more`.
fn mock_relationships(entries: Vec<Value>, more: bool) -> Value {
    mock_list_envelope(entries, more.then_some("next-page"))
}

/// A mock engine that knows the `Service` type and the item, with `relationships` around it.
async fn mock_engine(relationships: Value) -> MockEngine {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition("Service", "services", GROUP)],
            None,
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(RELATIONSHIPS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(relationships))
        .mount(engine.server())
        .await;

    engine
}

/// A mock engine with one inbound and one outbound relationship.
async fn mock_default_engine() -> MockEngine {
    mock_engine(mock_relationships(
        vec![mock_entry("outbound", 0), mock_entry("inbound", 1)],
        false,
    ))
    .await
}

/// Answers the delete.
async fn mount_delete(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("DELETE"))
        .and(path(ITEM_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// Runs the tool and renders its answer exactly as the runtime does — engine warnings included.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    let context = mock_context_at(&engine.server().uri(), CALL_BUDGET);
    let input: DeleteItemInput =
        serde_json::from_value(arguments).expect("the fixture arguments deserialise");

    let output = DeleteItem.call(&context, input).await?;

    Ok(output.render(context.engine().call_warnings().collected().as_deref()))
}

/// Every `DELETE` the mock received, as its query string.
async fn deletes(engine: &MockEngine) -> Vec<String> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.method == Method::DELETE)
        .map(|request| request.url.query().unwrap_or_default().to_string())
        .collect()
}

/// The fixture's arguments: the item by name and kind.
fn mock_arguments() -> Value {
    json!({ "name": MOCK_ITEM_NAME, "kind": "Service" })
}

// ---------------------------------------------------------------------------------------------
// The warning path, first. This is why the tool is not a passthrough.
// ---------------------------------------------------------------------------------------------

/// A `204` carrying the cascade warning is a delete that **succeeded** while its cleanup **failed**:
/// the answer says both, the warning verbatim.
#[rstest]
#[tokio::test]
async fn test_a_failed_cascade_is_reported_beside_the_delete() {
    let engine = mock_default_engine().await;
    mount_delete(
        &engine,
        ResponseTemplate::new(204)
            .append_header("Warning", format!(r#"299 - "{CASCADE_WARNING}""#).as_str()),
    )
    .await;

    let answer = run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["deleted"], json!(true));
    assert_eq!(answer["warnings"], json!([CASCADE_WARNING]));
}

/// With a clean cascade `warnings` is present and empty — its absence is never ambiguous.
#[rstest]
#[tokio::test]
async fn test_a_clean_delete_says_so_in_the_documented_shape() {
    let engine = mock_default_engine().await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(
        serde_json::to_string(&answer).expect("serialisable"),
        json!({
            "deleted": true, "name": MOCK_ITEM_NAME, "kind": "Service", "group": GROUP,
            "title": "Example Service", "relationshipsRemoved": 2, "warnings": []
        })
        .to_string()
    );
}

// ---------------------------------------------------------------------------------------------
// The token is always sent; a `409` is reported, once.
// ---------------------------------------------------------------------------------------------

/// The delete carries the pre-read's `resourceVersion`: never "whatever is there now".
#[rstest]
#[tokio::test]
async fn test_the_delete_carries_the_pre_reads_resource_version() {
    let engine = mock_default_engine().await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(deletes(&engine).await, vec!["resourceVersion=1"]);
}

/// A `409` is reported, **not** retried, and told apart from a `404`.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_reported_and_not_retried() {
    let engine = mock_default_engine().await;
    mount_delete(&engine, mock_failure(409, "Concurrent modification")).await;

    let error = run(&engine, mock_arguments())
        .await
        .expect_err("a conflict is an error");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, Remedy::RetryLater);
    assert!(
        error.message.contains("changed since it was read"),
        "{}",
        error.message
    );
    assert_eq!(deletes(&engine).await.len(), 1, "exactly one DELETE");
}

/// A `404` on the delete after the pre-read found the item: someone else deleted it in between.
/// That is not a wrong name, and it is not this call's success either.
#[rstest]
#[tokio::test]
async fn test_a_concurrent_delete_is_not_reported_as_ours() {
    let engine = mock_default_engine().await;
    mount_delete(&engine, mock_failure(404, "Item not found for deletion")).await;

    let error = run(&engine, mock_arguments())
        .await
        .expect_err("nothing was deleted");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::Escalate);
    assert!(
        error.message.contains("deleted by someone else"),
        "{}",
        error.message
    );
}

// ---------------------------------------------------------------------------------------------
// Never guess on a delete.
// ---------------------------------------------------------------------------------------------

/// A name that matches items of several types, with no `kind`, answers the candidates and issues
/// **no** `DELETE` at all.
#[rstest]
#[tokio::test]
async fn test_an_ambiguous_name_deletes_nothing() {
    let engine = mock_default_engine().await;
    let mut other = mock_item(MOCK_ITEM_NAME);
    other["apiVersion"] = json!("other.example.com/v1");
    other["kind"] = json!("Component");
    other["metadata"]["family"] = json!("components");
    Mock::given(method("GET"))
        .and(path(ITEMS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item(MOCK_ITEM_NAME), other],
            None,
        )))
        .mount(engine.server())
        .await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let error = run(&engine, json!({ "name": MOCK_ITEM_NAME }))
        .await
        .expect_err("an ambiguous name is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error
            .details
            .as_deref()
            .and_then(|details| details["candidates"].as_array())
            .map(Vec::len),
        Some(2)
    );
    assert!(
        error
            .next_step
            .as_deref()
            .is_some_and(|step| step.contains("delete_item again with `kind`"))
    );
    assert!(
        deletes(&engine).await.is_empty(),
        "nothing is deleted on a guess"
    );
}

/// A name that matches exactly one item is deleted without a `kind`.
#[rstest]
#[tokio::test]
async fn test_a_unique_name_needs_no_kind() {
    let engine = mock_default_engine().await;
    Mock::given(method("GET"))
        .and(path(ITEMS_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![mock_item(MOCK_ITEM_NAME)], None)),
        )
        .mount(engine.server())
        .await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, json!({ "name": MOCK_ITEM_NAME }))
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["kind"], json!("Service"));
    assert_eq!(deletes(&engine).await.len(), 1);
}

/// A wrong name — the pre-read's `404` — is `not_found` with near matches, never a fabricated
/// success, and nothing is deleted.
#[rstest]
#[tokio::test]
async fn test_a_wrong_name_offers_near_matches() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition("Service", "services", GROUP)],
            None,
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path("/stable.example.com/v1/items/services/example-iten"))
        .respond_with(mock_failure(404, "not found"))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(ITEMS_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![mock_item(MOCK_ITEM_NAME)], None)),
        )
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "name": "example-iten", "kind": "Service" }),
    )
    .await
    .expect_err("a wrong name is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"][0]["name"].clone()),
        Some(json!(MOCK_ITEM_NAME))
    );
    assert!(deletes(&engine).await.is_empty());
}

/// An item whose type no longer exists cannot be routed: `unaddressable_item`, nothing deleted.
#[rstest]
#[tokio::test]
async fn test_an_item_without_a_family_is_unaddressable() {
    let engine = MockEngine::start().await;
    let mut orphan = mock_item(MOCK_ITEM_NAME);
    orphan["metadata"]["family"] = Value::Null;
    Mock::given(method("GET"))
        .and(path(ITEMS_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![orphan], None)),
        )
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "name": MOCK_ITEM_NAME }))
        .await
        .expect_err("an orphan cannot be addressed");

    assert_eq!(error.code, codes::UNADDRESSABLE_ITEM);
    assert_eq!(error.remedy, Remedy::Escalate);
    assert!(deletes(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The blast radius.
// ---------------------------------------------------------------------------------------------

/// Inbound and outbound are both counted — other items lose their links too.
#[rstest]
#[tokio::test]
async fn test_relationships_are_counted_in_both_directions() {
    let engine = mock_engine(mock_relationships(
        vec![
            mock_entry("outbound", 0),
            mock_entry("inbound", 1),
            mock_entry("inbound", 2),
        ],
        false,
    ))
    .await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["relationshipsRemoved"], json!(3));

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");
    let count = requests
        .iter()
        .find(|request| request.url.path() == RELATIONSHIPS_PATH)
        .expect("the relationships were counted");
    assert_eq!(
        count.url.query(),
        Some(format!("limit={RELATIONSHIP_COUNT_PAGE}").as_str()),
        "one page, both directions"
    );
}

/// More than one page is a lower bound, never a walk.
#[rstest]
#[tokio::test]
async fn test_more_than_one_page_is_reported_as_a_lower_bound() {
    let engine = mock_engine(mock_relationships(vec![mock_entry("outbound", 0)], true)).await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["relationshipsRemoved"], json!("200+"));
}

/// No relationships is `0`, not absent.
#[rstest]
#[tokio::test]
async fn test_no_relationships_is_zero() {
    let engine = mock_engine(mock_relationships(vec![], false)).await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["relationshipsRemoved"], json!(0));
}

/// A count that fails does not stop the delete: the answer says the count is unknown, and why.
#[rstest]
#[tokio::test]
async fn test_a_failed_count_degrades_the_answer_not_the_delete() {
    let engine = mock_engine(json!({})).await;
    Mock::given(method("GET"))
        .and(path(RELATIONSHIPS_PATH))
        .respond_with(mock_failure(503, "unavailable"))
        .with_priority(1)
        .mount(engine.server())
        .await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let answer = run(&engine, mock_arguments())
        .await
        .expect("the delete succeeded");

    assert_eq!(answer["deleted"], json!(true));
    assert_eq!(answer["relationshipsRemoved"], Value::Null);
    assert!(
        answer["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("could not be counted")),
        "{answer}"
    );
    assert_eq!(deletes(&engine).await.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// The two `5XX` sides, which must not collapse.
// ---------------------------------------------------------------------------------------------

/// A failure **after** the `DELETE` left may have landed: verify before retrying.
#[rstest]
#[case::internal_error(500)]
#[case::unavailable(503)]
#[tokio::test]
async fn test_a_failed_delete_has_an_unknown_outcome(#[case] status: u16) {
    let engine = mock_default_engine().await;
    mount_delete(&engine, mock_failure(status, "Something went wrong")).await;

    let error = run(&engine, mock_arguments())
        .await
        .expect_err("a failed delete is an error");

    assert_eq!(error.code, codes::UNKNOWN_OUTCOME);
    assert_eq!(error.remedy, Remedy::Unknown);
}

/// A failure on the **pre-read** deleted nothing, and is safe to retry.
#[rstest]
#[case::internal_error(500)]
#[case::unavailable(503)]
#[tokio::test]
async fn test_a_failed_pre_read_deletes_nothing(#[case] status: u16) {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition("Service", "services", GROUP)],
            None,
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(ITEM_PATH))
        .respond_with(mock_failure(status, "Something went wrong"))
        .mount(engine.server())
        .await;

    let error = run(&engine, mock_arguments())
        .await
        .expect_err("a failed read is an error");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert_eq!(error.remedy, Remedy::Retry);
    assert!(deletes(&engine).await.is_empty());
}

/// An item read back without its `resourceVersion` is never deleted unguarded.
#[rstest]
#[tokio::test]
async fn test_an_item_without_a_resource_version_is_not_deleted() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition("Service", "services", GROUP)],
            None,
        )))
        .mount(engine.server())
        .await;
    let mut unversioned = mock_item(MOCK_ITEM_NAME);
    unversioned
        .as_object_mut()
        .map(|item| item.remove("resourceVersion"));
    Mock::given(method("GET"))
        .and(path(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(unversioned))
        .mount(engine.server())
        .await;
    mount_delete(&engine, ResponseTemplate::new(204)).await;

    let error = run(&engine, mock_arguments())
        .await
        .expect_err("an unguarded delete is refused");

    assert_eq!(error.code, codes::SERVER_DEFECT);
    assert!(deletes(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Arguments checked before anything reaches the engine.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::empty_name(json!({ "name": "" }), "name")]
#[case::long_name(json!({ "name": "a".repeat(MAX_NAME_BYTES + 1) }), "name")]
#[case::uppercase_name(json!({ "name": "Example-Item" }), "name")]
#[case::empty_kind(json!({ "name": "example-item", "kind": "" }), "kind")]
#[case::long_kind(json!({ "name": "example-item", "kind": "A".repeat(MAX_KIND_BYTES + 1) }), "kind")]
#[case::malformed_kind(json!({ "name": "example-item", "kind": "my-kind" }), "kind")]
#[case::group_without_kind(json!({ "name": "example-item", "group": "example.com" }), "group")]
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
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
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
