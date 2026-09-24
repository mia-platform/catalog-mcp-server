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
    client::{Deadline, EngineClientFactory, FailureKind, RetryPolicy},
    error::{Remedy, codes},
    ops::ListQuery,
    testing::{MockEngine, mock_identity, mock_item, mock_list_envelope},
};
use rstest::rstest;
use std::time::Duration;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path},
};

/// A policy shaped like the shipped defaults: one retry, 1 s connect, 5 s read.
fn mock_policy(max_retries: u8) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        connect_timeout: Duration::from_secs(1),
        timeout: Duration::from_secs(5),
    }
}

// ---------------------------------------------------------------------------------------------
// §8.1 — the retry policy. Four conditions, and a case for each of them failing.
// ---------------------------------------------------------------------------------------------

/// A `500` is **never** retried: the engine emits it for application faults, and an application
/// fault may have committed.
#[rstest]
fn test_a_500_is_not_retried() {
    assert!(!FailureKind::Status(500).is_retryable());
}

#[rstest]
#[case::bad_gateway(502)]
#[case::unavailable(503)]
#[case::gateway_timeout(504)]
fn test_a_gateway_status_is_retryable(#[case] status: u16) {
    assert!(FailureKind::Status(status).is_retryable());
}

#[rstest]
#[case::bad_request(400)]
#[case::not_found(404)]
#[case::conflict(409)]
#[case::not_implemented(501)]
fn test_a_4xx_is_never_retried(#[case] status: u16) {
    assert!(!FailureKind::Status(status).is_retryable());
}

#[rstest]
fn test_a_transport_failure_is_retryable() {
    assert!(FailureKind::Timeout.is_retryable());
    assert!(FailureKind::Connect.is_retryable());
}

/// **D20's dividing line.** A connect failure is the one case where a write is known not to have
/// happened; everything else leaves the outcome unknowable from here.
#[rstest]
fn test_only_a_connect_failure_leaves_a_write_certainly_unapplied() {
    assert!(!FailureKind::Connect.may_have_been_applied());
    assert!(FailureKind::Timeout.may_have_been_applied());
    assert!(FailureKind::Status(500).may_have_been_applied());
    assert!(FailureKind::Status(503).may_have_been_applied());
}

/// Condition one: idempotent by construction. A dispatched write is never retried (D20).
#[rstest]
fn test_a_non_idempotent_request_is_not_retried() {
    let deadline = Deadline::starting_now(Duration::from_secs(25));

    assert!(!mock_policy(1).allows(false, FailureKind::Timeout, 0, &deadline));
}

/// Condition four: below `maxRetries`.
#[rstest]
fn test_the_attempt_count_is_respected() {
    let deadline = Deadline::starting_now(Duration::from_secs(25));
    let policy = mock_policy(1);

    assert!(policy.allows(true, FailureKind::Timeout, 0, &deadline));
    assert!(!policy.allows(true, FailureKind::Timeout, 1, &deadline));
}

#[rstest]
fn test_zero_retries_means_zero() {
    let deadline = Deadline::starting_now(Duration::from_secs(25));

    assert!(!mock_policy(0).allows(true, FailureKind::Timeout, 0, &deadline));
}

/// Condition three: the deadline must have room for a **full** further attempt — connect plus
/// read — or the retry only turns a clean failure into a deadline one.
#[rstest]
#[tokio::test]
async fn test_a_retry_that_would_exceed_the_deadline_is_not_attempted() {
    tokio::time::pause();
    let deadline = Deadline::starting_now(Duration::from_secs(25));
    let policy = mock_policy(1);

    assert!(policy.allows(true, FailureKind::Timeout, 0, &deadline));

    // Six seconds left is exactly connect + read, and the condition is strictly greater.
    tokio::time::advance(Duration::from_secs(19)).await;

    assert!(!policy.allows(true, FailureKind::Timeout, 0, &deadline));
}

// ---------------------------------------------------------------------------------------------
// The base URL.
// ---------------------------------------------------------------------------------------------

/// `Url::join` discards the last segment of a base that does not end in `/`, which would drop
/// `/api/catalog` from every request. The prefix is joined once, here, and this is the guard.
#[rstest]
#[case::plain("http://api-gateway:8080", "/api/catalog")]
#[case::trailing_slash_on_base("http://api-gateway:8080/", "/api/catalog")]
#[case::trailing_slash_on_prefix("http://api-gateway:8080", "/api/catalog/")]
#[case::no_leading_slash("http://api-gateway:8080", "api/catalog")]
fn test_the_api_prefix_survives_the_join(#[case] base_url: &str, #[case] api_prefix: &str) {
    let factory = EngineClientFactory::new(
        base_url,
        api_prefix,
        Duration::from_secs(5),
        Duration::from_secs(1),
        1,
    )
    .expect("a well-formed base URL");

    let client = factory.bind(
        mock_identity(),
        Deadline::starting_now(Duration::from_secs(25)),
    );

    let url = client.url(["items"]).expect("a well-formed URL");

    assert_eq!(url.as_str(), "http://api-gateway:8080/api/catalog/items");
}

#[rstest]
fn test_an_empty_api_prefix_is_allowed() {
    let factory = EngineClientFactory::new(
        "http://api-gateway:8080",
        "/",
        Duration::from_secs(5),
        Duration::from_secs(1),
        1,
    )
    .expect("a well-formed base URL");

    assert_eq!(factory.base_url().as_str(), "http://api-gateway:8080/");
}

/// D24 — segments are pushed, never formatted, so nothing can introduce a path separator.
#[rstest]
#[tokio::test]
async fn test_path_segments_are_percent_encoded() {
    let engine = MockEngine::start().await;
    let client = engine.client(mock_identity());

    let url = client.url(["items", "a/b c"]).expect("a well-formed URL");

    assert!(url.path().ends_with("/items/a%2Fb%20c"));
}

// ---------------------------------------------------------------------------------------------
// The send path, against the mock engine.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_a_successful_read_is_shaped_and_carries_no_warnings() {
    let engine = MockEngine::start().await;
    engine
        .get_ok(
            "/items",
            mock_list_envelope(vec![mock_item("example-item")], None),
        )
        .await;

    let response = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect("the listing succeeds");

    assert_eq!(response.value.items.len(), 1);
    assert_eq!(response.value.items[0].metadata.name, "example-item");
    assert_eq!(response.value.next, None);
    assert!(response.warnings.is_empty());
}

/// P6 — **every** engine response passes through the warning parser, on the success path too.
#[rstest]
#[tokio::test]
async fn test_warnings_are_collected_from_a_successful_response() {
    let engine = MockEngine::start().await;
    engine
        .get_ok_with_warnings(
            "/items",
            mock_list_envelope(vec![], None),
            &["first warning", "second warning"],
        )
        .await;

    let response = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect("the listing succeeds");

    let texts: Vec<&str> = response
        .warnings
        .iter()
        .map(|warning| warning.text.as_str())
        .collect();

    assert_eq!(texts, vec!["first warning", "second warning"]);
}

/// Texts of what the call collected, or `None` when it never reached for the engine.
fn collected_texts(client: &crate::EngineClient) -> Option<Vec<String>> {
    client
        .call_warnings()
        .collected()
        .map(|warnings| warnings.into_iter().map(|warning| warning.text).collect())
}

/// D28 — a call that never reached for the engine has nothing to report, so the key is omitted.
#[rstest]
#[tokio::test]
async fn test_a_call_that_never_reached_the_engine_collects_nothing() {
    let engine = MockEngine::start().await;
    let client = engine.client(mock_identity());

    assert_eq!(collected_texts(&client), None);
}

/// D28 — a call that reached the engine and was told nothing collects an **empty** list, which
/// is what makes the key present-and-empty rather than absent.
#[rstest]
#[tokio::test]
async fn test_a_call_without_warnings_collects_an_empty_list() {
    let engine = MockEngine::start().await;
    engine
        .get_ok("/items", mock_list_envelope(vec![], None))
        .await;
    let client = engine.client(mock_identity());

    client
        .list_items(&ListQuery::default())
        .await
        .expect("the listing succeeds");

    assert_eq!(collected_texts(&client), Some(vec![]));
}

/// §5.5 — the runtime collects every warning of every response, across clones of the call's
/// client, and repeats none: a tool that fans out reports each distinct warning once.
#[rstest]
#[tokio::test]
async fn test_warnings_are_collected_across_calls_and_clones_without_repeats() {
    let engine = MockEngine::start().await;
    engine
        .get_ok_with_warnings(
            "/items",
            mock_list_envelope(vec![], None),
            &["type is deprecated", "history is frozen"],
        )
        .await;
    let client = engine.client(mock_identity());
    let fan_out = client.clone();

    for caller in [&client, &fan_out] {
        caller
            .list_items(&ListQuery::default())
            .await
            .expect("the listing succeeds");
    }

    assert_eq!(
        collected_texts(&client),
        Some(vec![
            "type is deprecated".to_string(),
            "history is frozen".to_string()
        ])
    );
}

/// A failed request still counts as reaching for the engine: the tool that made it could have
/// been warned, so a success built around it keeps the key.
#[rstest]
#[tokio::test]
async fn test_a_failed_request_still_marks_the_engine_as_called() {
    let engine = MockEngine::start().await;
    engine.get_error("/items", 404, "not here").await;
    let client = engine.client_without_retries(mock_identity());

    client
        .list_items(&ListQuery::default())
        .await
        .expect_err("the listing fails");

    assert_eq!(collected_texts(&client), Some(vec![]));
}

#[rstest]
#[tokio::test]
async fn test_an_engine_error_is_mapped_to_the_contract() {
    let engine = MockEngine::start().await;
    engine
        .get_error("/items", 404, "No item type matches this request.")
        .await;

    let error = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect_err("a 404 is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(error.message, "No item type matches this request.");
}

/// The engine's `500` body says only *"Something went wrong"*, so the message is ours and its
/// `x-request-id` is the one actionable thing a human gets.
#[rstest]
#[tokio::test]
async fn test_a_500_carries_our_message_and_the_engine_request_id() {
    let engine = MockEngine::start().await;
    engine
        .get_error("/items", 500, "Something went wrong")
        .await;

    let error = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect_err("a 500 is an error");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert!(error.message.contains("not the same as an empty result"));
    assert_eq!(
        error.details.expect("the request id is carried")["requestId"],
        serde_json::json!("engine-request-0001")
    );
}

/// A `503` is retried exactly once, and the second answer is the one that counts.
#[rstest]
#[tokio::test]
async fn test_a_503_is_retried_once_and_then_succeeds() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(engine.server())
        .await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .expect(1)
        .mount(engine.server())
        .await;

    let response = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect("the retry succeeds");

    assert!(response.value.items.is_empty());
}

/// With retries switched off, the first `503` is the answer — which is also the assertion that
/// the retry above really was the policy and not a client default.
#[rstest]
#[tokio::test]
async fn test_a_503_is_not_retried_when_the_policy_forbids_it() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(engine.server())
        .await;

    let error = engine
        .client_without_retries(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect_err("a 503 with no retries left is an error");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
}

/// A `500` must reach the caller on the first attempt: retrying it is what §8.1 forbids.
#[rstest]
#[tokio::test]
async fn test_a_500_is_answered_on_the_first_attempt() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(crate::testing::mock_error_body(500, "Something went wrong")),
        )
        .expect(1)
        .mount(engine.server())
        .await;

    let error = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect_err("a 500 is an error");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
}

/// A body we cannot read means the engine's shape and ours have diverged. That is **our**
/// problem, so the model is told to escalate rather than sent round a loop it cannot win.
#[rstest]
#[tokio::test]
async fn test_an_unreadable_body_is_reported_as_our_defect() {
    let engine = MockEngine::start().await;
    engine
        .get_ok("/items", serde_json::json!({ "unexpected": "shape" }))
        .await;

    let error = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect_err("an unreadable body is an error");

    assert_eq!(error.code, codes::SERVER_DEFECT);
    assert_eq!(error.remedy, Remedy::Escalate);
}

/// A deadline with no time left fails **without dialling**: the mock would otherwise record a
/// request, and `expect(0)` is what asserts it did not.
#[rstest]
#[tokio::test(start_paused = true)]
async fn test_an_expired_deadline_fails_without_dialling() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(engine.server())
        .await;

    let client = engine.client_with_deadline(mock_identity(), Duration::from_secs(1));
    tokio::time::advance(Duration::from_secs(5)).await;

    let error = client
        .list_items(&ListQuery::default())
        .await
        .expect_err("an expired deadline is an error");

    assert_eq!(error.code, codes::DEADLINE_EXCEEDED);
    assert_eq!(error.remedy, Remedy::Retry);
}
