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
    registry::contract::{CallContext, Tool, ToolOutput},
    tools::tag_item_version::{MAX_DESCRIPTION_BYTES, TagItemVersion, TagItemVersionInput},
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item, mock_item_type_definition,
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
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The fixture item.
const ITEM_PATH: &str = "/stable.example.com/v1/items/services/example-item";

/// Its revisions.
const REVISIONS_PATH: &str = "/stable.example.com/v1/items/services/example-item/revisions";

/// Its versions.
const VERSIONS_PATH: &str = "/stable.example.com/v1/items/services/example-item/versions";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The caller, as the gateway would name it.
const CALLER: &str = "019f1cb8-12ec-7f43-94d4-285b77930ce7";

/// Someone else.
const OTHER: &str = "019f1cb8-0000-7000-8000-000000000009";

/// The newest revision of the fixture item.
const NEWEST: &str = "01998f1e-0000-7000-8000-000000000001";

/// An older one.
const OLDER: &str = "01998f1e-0000-7000-8000-000000000002";

/// When the newest revision was recorded.
const RECORDED_AT: &str = "2026-09-18T10:22:03Z";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// A call context against `engine`, as the caller.
fn mock_context(engine: &MockEngine) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        Some(CALLER),
        Some("Bearer test-token"),
        None,
    ));
    let client = EngineClientFactory::new(
        &engine.server().uri(),
        "/",
        Duration::from_secs(5),
        Duration::from_millis(200),
        0,
    )
    .expect("the fixture URL is well formed")
    .bind(identity.clone(), Deadline::starting_now(CALL_BUDGET));

    CallContext::new(
        client,
        Deadline::starting_now(CALL_BUDGET),
        CancellationToken::new(),
        None,
        identity.tenant_key(),
    )
}

/// Runs the tool, returning its whole output.
async fn run_with(engine: &MockEngine, arguments: Value) -> Result<ToolOutput, ToolError> {
    let input: TagItemVersionInput =
        serde_json::from_value(arguments).expect("the fixture arguments deserialise");

    TagItemVersion.call(&mock_context(engine), input).await
}

/// Runs the tool, returning its payload.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    run_with(engine, arguments)
        .await
        .map(|output| output.payload().clone())
}

/// A call tagging the fixture item as `version_name`, with `extra` arguments.
fn mock_arguments(version_name: &str, extra: Value) -> Value {
    let mut arguments =
        json!({ "name": "example-item", "kind": "Service", "versionName": version_name });
    if let Value::Object(extra) = extra {
        arguments.as_object_mut().expect("an object").extend(extra);
    }
    arguments
}

/// A version named `name` tagging `revision`, cut by `author`.
fn mock_version(name: &str, revision: &str, author: &str) -> Value {
    json!({
        "apiVersion": "mia-platform.eu/v1", "kind": "ItemVersion",
        "metadata": { "name": name, "creationTimestamp": "2026-09-18T10:30:45Z",
                      "author": { "id": author, "displayName": "Ada Lovelace" } },
        "spec": { "revisionId": revision }
    })
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// A mock engine knowing `definition` as `Service`.
async fn mock_engine_with(definition: Value) -> MockEngine {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![definition], None)),
        )
        .mount(engine.server())
        .await;

    engine
}

/// A mock engine knowing the fixture type, recording on.
async fn mock_engine() -> MockEngine {
    mock_engine_with(mock_item_type_definition("Service", "services", GROUP)).await
}

/// Answers the revision read with `revisions` (uid, time) and the item read with an item last
/// changed at `updated`.
async fn mount_now(engine: &MockEngine, revisions: &[(&str, &str)], updated: &str) {
    let items: Vec<Value> = revisions
        .iter()
        .map(|(uid, time)| {
            json!({ "apiVersion": "mia-platform.eu/v1", "kind": "ItemRevision",
                    "metadata": { "uid": uid, "creationTimestamp": time },
                    "spec": { "origin": "Observed" } })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(REVISIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(items, None)))
        .mount(engine.server())
        .await;

    let mut item = mock_item("example-item");
    item["metadata"]["updateTimestamp"] = json!(updated);
    engine.get_ok(ITEM_PATH, item).await;
}

/// Answers the tag with `response`.
async fn mount_tag(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(VERSIONS_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Answers the versions listing with one page of `versions`.
async fn mount_versions(engine: &MockEngine, versions: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(VERSIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(versions, None)))
        .mount(engine.server())
        .await;
}

/// Every request of `verb` on `url_path`.
async fn requests_on(engine: &MockEngine, verb: Method, url_path: &str) -> Vec<wiremock::Request> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.method == verb && request.url.path() == url_path)
        .collect()
}

/// The error's details.
fn details(error: &ToolError) -> Value {
    error.details.as_deref().cloned().unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// The tag.
// ---------------------------------------------------------------------------------------------

/// The item as it stands: the newest revision, checked against the item, sent as JSON with no query.
#[rstest]
#[tokio::test]
async fn test_the_item_as_it_stands_is_tagged() {
    let engine = mock_engine().await;
    mount_now(&engine, &[(NEWEST, RECORDED_AT)], RECORDED_AT).await;
    mount_tag(
        &engine,
        ResponseTemplate::new(201).set_body_json(mock_version("1.4.0", NEWEST, CALLER)),
    )
    .await;

    let output = run_with(
        &engine,
        mock_arguments("1.4.0", json!({ "description": "Before the move" })),
    )
    .await
    .expect("tagged");

    assert_eq!(
        output.payload(),
        &json!({
            "tagged": true, "name": "example-item", "kind": "Service", "group": GROUP,
            "version": "v1", "family": "services", "versionName": "1.4.0",
            "revision": NEWEST, "revisionResolved": true, "revisionRecordedAt": RECORDED_AT,
            "taggedAt": "2026-09-18T10:30:45Z", "taggedBy": "Ada Lovelace", "taggedById": CALLER,
        })
    );
    let tags = requests_on(&engine, Method::POST, VERSIONS_PATH).await;
    assert_eq!(tags.len(), 1, "never retried");
    assert_eq!(tags[0].url.query(), None);
    let body: Value = serde_json::from_slice(&tags[0].body).expect("a JSON body");
    assert_eq!(
        body,
        json!({ "name": "1.4.0", "revisionId": NEWEST, "description": "Before the move" })
    );
    assert_eq!(
        requests_on(&engine, Method::GET, REVISIONS_PATH).await[0]
            .url
            .query(),
        Some("limit=1")
    );
    assert!(output.render(None).get("warnings").is_none());
}

/// An item changed after its newest revision is tagged as that revision, with a warning.
#[rstest]
#[tokio::test]
async fn test_a_stale_newest_revision_is_warned_about() {
    let engine = mock_engine().await;
    mount_now(&engine, &[(NEWEST, RECORDED_AT)], "2026-09-18T11:00:00Z").await;
    mount_tag(
        &engine,
        ResponseTemplate::new(201).set_body_json(mock_version("1.4.0", NEWEST, CALLER)),
    )
    .await;

    let output = run_with(&engine, mock_arguments("1.4.0", json!({})))
        .await
        .expect("tagged");

    assert_eq!(
        output.render(None)["warnings"],
        json!([super::STALE_WARNING])
    );
}

/// A given revision is sent as it is, with no read.
#[rstest]
#[tokio::test]
async fn test_a_given_revision_needs_no_read() {
    let engine = mock_engine().await;
    mount_tag(
        &engine,
        ResponseTemplate::new(201).set_body_json(mock_version("1.3.0", OLDER, CALLER)),
    )
    .await;

    let payload = run(
        &engine,
        mock_arguments("1.3.0", json!({ "revision": OLDER })),
    )
    .await
    .expect("tagged");

    assert_eq!(payload["revision"], json!(OLDER));
    assert_eq!(payload["revisionResolved"], json!(false));
    assert!(payload.get("revisionRecordedAt").is_none());
    assert!(
        requests_on(&engine, Method::GET, REVISIONS_PATH)
            .await
            .is_empty()
    );
    assert!(
        requests_on(&engine, Method::GET, ITEM_PATH)
            .await
            .is_empty()
    );
}

/// An answer carrying only the version's name is still a success: the tag exists.
#[rstest]
#[tokio::test]
async fn test_a_sparse_answer_is_still_a_success() {
    let engine = mock_engine().await;
    mount_tag(
        &engine,
        ResponseTemplate::new(201).set_body_json(json!({ "metadata": { "name": "1.3.0" } })),
    )
    .await;

    let payload = run(
        &engine,
        mock_arguments("1.3.0", json!({ "revision": OLDER })),
    )
    .await
    .expect("tagged");

    assert_eq!(payload["tagged"], json!(true));
    assert!(payload.get("taggedBy").is_none());
}

/// An item with no recorded revision has nothing to tag yet: writing it records one.
#[rstest]
#[tokio::test]
async fn test_no_revision_yet() {
    let engine = mock_engine().await;
    mount_now(&engine, &[], RECORDED_AT).await;

    let error = run(&engine, mock_arguments("1.0.0", json!({})))
        .await
        .expect_err("nothing");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("write the item"))
    );
    assert!(
        requests_on(&engine, Method::POST, VERSIONS_PATH)
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------------------------
// What cannot record, before any call.
// ---------------------------------------------------------------------------------------------

/// A type with recording off is refused from its definition, with the setting named.
#[rstest]
#[tokio::test]
async fn test_recording_off_is_refused_before_any_call() {
    let mut definition = mock_item_type_definition("Service", "services", GROUP);
    definition["spec"]["history"] = json!({ "enabled": false });
    let engine = mock_engine_with(definition).await;

    let error = run(&engine, mock_arguments("1.0.0", json!({})))
        .await
        .expect_err("off");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate)
    );
    assert_eq!(details(&error)["permanent"], json!(false));
    assert!(error.message.contains("switch it on"));
    assert!(
        requests_on(&engine, Method::GET, REVISIONS_PATH)
            .await
            .is_empty()
    );
    assert!(
        requests_on(&engine, Method::POST, VERSIONS_PATH)
            .await
            .is_empty()
    );
}

/// One of the four kinds that can never record is refused as permanent.
#[rstest]
#[tokio::test]
async fn test_a_kind_that_can_never_record_is_refused() {
    let engine = mock_engine_with(mock_item_type_definition(
        "Service",
        "relationships",
        "mia-platform.eu",
    ))
    .await;

    let error = run(&engine, mock_arguments("1.0.0", json!({})))
        .await
        .expect_err("never");

    assert_eq!(details(&error)["permanent"], json!(true));
    assert!(!error.message.contains("switch it on"));
}

/// An engine `422` despite the type's setting — it changed in between — is classified by its
/// phrase; another is relayed.
#[rstest]
#[case::not_recorded(
    "Revision history is not being recorded for items of family 'services' in API group \
     'stable.example.com/v1': set 'spec.history.enabled' on its Item Type Definition to resume",
    Some(false)
)]
#[case::other("Something else entirely", None)]
#[tokio::test]
async fn test_an_engine_refusal_is_classified(
    #[case] message: &str,
    #[case] permanent: Option<bool>,
) {
    let engine = mock_engine().await;
    mount_tag(&engine, mock_failure(422, message)).await;

    let error = run(
        &engine,
        mock_arguments("1.0.0", json!({ "revision": OLDER })),
    )
    .await
    .expect_err("refused");

    assert_eq!(error.code, codes::UNSUPPORTED_FOR_TYPE);
    match permanent {
        Some(permanent) => assert_eq!(details(&error)["permanent"], json!(permanent)),
        None => assert_eq!(error.message, message),
    }
}

// ---------------------------------------------------------------------------------------------
// A taken name.
// ---------------------------------------------------------------------------------------------

/// The same name on the same revision is this very tag: a repeat, recognised as done, saying
/// whether it was this caller's.
#[rstest]
#[case::by_the_caller(CALLER, true)]
#[case::by_someone_else(OTHER, false)]
#[tokio::test]
async fn test_a_repeat_is_recognised(#[case] author: &str, #[case] by_you: bool) {
    let engine = mock_engine().await;
    mount_tag(
        &engine,
        mock_failure(409, "A version with name '1.3.0' already exists"),
    )
    .await;
    mount_versions(&engine, vec![mock_version("1.3.0", OLDER, author)]).await;

    let payload = run(
        &engine,
        mock_arguments("1.3.0", json!({ "revision": OLDER })),
    )
    .await
    .expect("already tagged");

    assert_eq!(payload["tagged"], json!(false));
    assert_eq!(payload["alreadyTagged"], json!(true));
    assert_eq!(payload["taggedByYou"], json!(by_you));
    assert_eq!(payload["taggedAt"], json!("2026-09-18T10:30:45Z"));
}

/// The same name on another revision is a clash: change the name — with the nearest free one,
/// the names taken, highest first, and how many there are.
#[rstest]
#[tokio::test]
async fn test_a_taken_name_suggests_a_free_one() {
    let engine = mock_engine().await;
    mount_tag(
        &engine,
        mock_failure(409, "A version with name '1.4.0' already exists"),
    )
    .await;
    mount_versions(
        &engine,
        vec![
            mock_version("1.4.1", OLDER, OTHER),
            mock_version("1.4.0", OLDER, OTHER),
        ],
    )
    .await;

    let error = run(
        &engine,
        mock_arguments("1.4.0", json!({ "revision": NEWEST })),
    )
    .await
    .expect_err("taken");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CONFLICT, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains("never renamed"), "{}", error.message);
    assert_eq!(details(&error)["suggestedVersion"], json!("1.4.2"));
    assert_eq!(details(&error)["existing"], json!(["1.4.1", "1.4.0"]));
    assert_eq!(details(&error)["existingCount"], json!(2));
}

/// The versions are listed every page, only on the clash.
#[rstest]
#[tokio::test]
async fn test_every_page_of_versions_is_read() {
    let engine = mock_engine().await;
    mount_tag(&engine, mock_failure(409, "taken")).await;
    Mock::given(method("GET"))
        .and(path(VERSIONS_PATH))
        .and(query_param_is_missing("continue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_version("2.0.0", NEWEST, OTHER)],
            Some("page-2"),
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(VERSIONS_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_version("1.0.0", OLDER, CALLER)],
            None,
        )))
        .mount(engine.server())
        .await;

    let payload = run(
        &engine,
        mock_arguments("1.0.0", json!({ "revision": OLDER })),
    )
    .await
    .expect("the repeat is on page 2");

    assert_eq!(payload["alreadyTagged"], json!(true));
    assert_eq!(
        requests_on(&engine, Method::GET, VERSIONS_PATH).await.len(),
        2
    );
}

/// When the versions cannot be listed, the clash keeps its own words — never a concurrent edit's.
#[rstest]
#[tokio::test]
async fn test_a_clash_without_the_listing_keeps_its_words() {
    let engine = mock_engine().await;
    mount_tag(&engine, mock_failure(409, "taken")).await;
    Mock::given(method("GET"))
        .and(path(VERSIONS_PATH))
        .respond_with(mock_failure(403, "no"))
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        mock_arguments("1.0.0", json!({ "revision": OLDER })),
    )
    .await
    .expect_err("taken");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CONFLICT, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains("versionName"));
    assert!(!error.message.contains("in flight"));
    assert!(details(&error).get("suggestedVersion").is_none());
}

// ---------------------------------------------------------------------------------------------
// The other refusals.
// ---------------------------------------------------------------------------------------------

/// A `404` is told apart by reading the item, never by its wording.
#[rstest]
#[case::resolved_then_gone(None, None, "call again")]
#[case::given_item_missing(Some(OLDER), Some(404), "describe_item")]
#[case::given_revision_missing(Some(OLDER), Some(200), "omit `revision`")]
#[tokio::test]
async fn test_a_not_found_is_told_apart(
    #[case] revision: Option<&str>,
    #[case] item_status: Option<u16>,
    #[case] step: &str,
) {
    let engine = mock_engine().await;
    if revision.is_none() {
        mount_now(&engine, &[(NEWEST, RECORDED_AT)], RECORDED_AT).await;
    }
    match item_status {
        Some(200) => engine.get_ok(ITEM_PATH, mock_item("example-item")).await,
        Some(status) => {
            Mock::given(method("GET"))
                .and(path(ITEM_PATH))
                .respond_with(mock_failure(status, "missing"))
                .mount(engine.server())
                .await;
        }
        None => {}
    }
    mount_tag(&engine, mock_failure(404, "Revision not found")).await;

    let arguments = match revision {
        Some(revision) => mock_arguments("1.0.0", json!({ "revision": revision })),
        None => mock_arguments("1.0.0", json!({})),
    };
    let error = run(&engine, arguments).await.expect_err("not found");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert!(error.next_step.is_some_and(|next| next.contains(step)));
}

/// The gateway's `403` is explained from the item: an AI Foundry item, a built-in name, or neither.
#[rstest]
#[case::ai_foundry("ai.mia-platform.eu", "example-item", "AI Foundry")]
#[case::built_in(GROUP, "catalog.mia-platform.eu", "Built-in")]
#[case::other(GROUP, "example-item", "not permitted")]
#[tokio::test]
async fn test_a_forbidden_tag_is_explained(
    #[case] group: &str,
    #[case] name: &str,
    #[case] says: &str,
) {
    let engine = mock_engine_with(mock_item_type_definition("Service", "services", group)).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403))
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "name": name, "kind": "Service", "versionName": "1.0.0", "revision": OLDER }),
    )
    .await
    .expect_err("forbidden");

    assert_eq!(
        (error.code, error.remedy),
        (codes::FORBIDDEN, Remedy::Escalate)
    );
    assert!(error.message.contains(says), "{}", error.message);
}

/// A tag whose answer was lost may exist: calling again is safe, since a repeat is recognised.
#[rstest]
#[tokio::test]
async fn test_a_lost_answer_is_an_unknown_outcome() {
    let engine = mock_engine().await;
    mount_tag(&engine, ResponseTemplate::new(503)).await;

    let error = run(
        &engine,
        mock_arguments("1.0.0", json!({ "revision": OLDER })),
    )
    .await
    .expect_err("unknown");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNKNOWN_OUTCOME, Remedy::Unknown)
    );
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("recognised"))
    );
    assert_eq!(
        requests_on(&engine, Method::POST, VERSIONS_PATH)
            .await
            .len(),
        1
    );
}

/// The input bounds: refused before anything is sent, naming the parameter.
#[rstest]
#[case::api_version(mock_arguments("v1", json!({})), "versionName")]
#[case::not_semver(mock_arguments("latest", json!({})), "versionName")]
#[case::bad_revision(mock_arguments("1.0.0", json!({ "revision": "r-1" })), "revision")]
#[case::long_description(
    mock_arguments("1.0.0", json!({ "description": "x".repeat(MAX_DESCRIPTION_BYTES + 1) })),
    "description"
)]
#[case::bad_name(json!({ "name": "Not A Name", "kind": "Service", "versionName": "1.0.0" }), "name")]
#[tokio::test]
async fn test_a_broken_bound_is_refused_before_sending(
    #[case] arguments: Value,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&engine, arguments).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(details(&error)["field"], json!(field));
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .expect("the mock records its requests")
            .is_empty()
    );
}
