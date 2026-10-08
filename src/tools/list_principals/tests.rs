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
    tools::list_principals::{ListPrincipals, ListPrincipalsInput, MAX_ROWS, TOOL_NAME},
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MOCK_PRINCIPAL_ID, MockEngine, mock_acl_context, mock_error_body, mock_list_envelope,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The caller's own identity route.
const ME_PATH: &str = "/bff/me";

/// The principal directory route.
const PRINCIPALS_PATH: &str = "/bff/principals";

/// A principal id the tests own.
const ADA_ID: &str = "523f5c33-1a5c-7270-aa81-bbc05ab201dc";

/// A user as the directory lists one.
fn mock_user(id: &str, display_name: &str, email: &str) -> Value {
    json!({ "id": id, "type": "user", "displayName": display_name, "email": email })
}

/// A service account as the directory lists one: no e-mail.
fn mock_service_account(id: &str, display_name: &str) -> Value {
    json!({ "id": id, "type": "serviceAccount", "displayName": display_name })
}

/// `n` users, `user-0` … named `User <i>` with `user<i>@example.com`.
fn mock_users(prefix: &str, n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            mock_user(
                &format!("{prefix}-{i}"),
                &format!("User {prefix} {i}"),
                &format!("{prefix}{i}@example.com"),
            )
        })
        .collect()
}

/// A call context pointed at `engine`, forwarding the fixture identity.
fn mock_context(engine: &MockEngine) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        Some(MOCK_PRINCIPAL_ID),
        Some("Bearer test-token"),
        None,
    ));

    let client = EngineClientFactory::new(
        &engine.server().uri(),
        "/",
        Duration::from_secs(5),
        Duration::from_secs(1),
        0,
    )
    .expect("the mock engine URL is well formed")
    .bind(
        identity.clone(),
        Deadline::starting_now(Duration::from_secs(25)),
    );

    CallContext::new(
        client,
        Deadline::starting_now(Duration::from_secs(25)),
        CancellationToken::new(),
        None,
        identity.tenant_key(),
    )
}

/// The input, deserialised as the runtime deserialises a call's arguments.
fn mock_input(arguments: Value) -> ListPrincipalsInput {
    serde_json::from_value(arguments).expect("the fixture arguments deserialise")
}

/// Runs the tool against `engine`.
async fn run(engine: &MockEngine, arguments: Value) -> Result<ToolOutput, ToolError> {
    ListPrincipals
        .call(&mock_context(engine), mock_input(arguments))
        .await
}

/// Mounts `/bff/me` answering `body`.
async fn mock_me(body: Value) -> MockEngine {
    let engine = MockEngine::start().await;
    engine.get_ok(ME_PATH, body).await;

    engine
}

/// Mounts a directory of pages: page `i` is served for `continue=t<i>` (the first without one),
/// and points at `t<i+1>` unless it is the last.
async fn mock_directory(pages: Vec<Vec<Value>>) -> MockEngine {
    let engine = MockEngine::start().await;
    let last = pages.len().saturating_sub(1);

    for (index, items) in pages.into_iter().enumerate() {
        let next = (index < last).then(|| format!("t{}", index + 1));
        let template =
            ResponseTemplate::new(200).set_body_json(mock_list_envelope(items, next.as_deref()));

        let mock = Mock::given(method("GET")).and(path(PRINCIPALS_PATH));
        let mock = if index == 0 {
            mock.and(query_param_is_missing("continue"))
        } else {
            mock.and(query_param("continue", format!("t{index}")))
        };

        mock.respond_with(template).mount(engine.server()).await;
    }

    engine
}

/// Mounts a directory answering every request with `status`.
async fn mock_failing(route: &str, status: u16, message: &str) -> MockEngine {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(mock_error_body(status, message)))
        .mount(engine.server())
        .await;

    engine
}

/// The query of every directory request, in order.
async fn sent_queries(engine: &MockEngine) -> Vec<HashMap<String, String>> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.url.path() == PRINCIPALS_PATH)
        .map(|request| request.url.query_pairs().into_owned().collect())
        .collect()
}

/// The `owner.ref` of every row.
fn refs(payload: &Value) -> Vec<String> {
    payload["principals"]
        .as_array()
        .expect("principals is an array")
        .iter()
        .map(|row| row["owner"]["ref"].as_str().unwrap_or_default().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The surface.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_the_descriptor_is_read_only_and_named() {
    let descriptor = ListPrincipals::descriptor();

    assert_eq!(descriptor.name, TOOL_NAME);
    assert_eq!(descriptor.annotations.read_only_hint, Some(true));
    assert!(descriptor.description.contains("metadata.owner"));
}

// ---------------------------------------------------------------------------------------------
// `me`.
// ---------------------------------------------------------------------------------------------

/// The caller as one row: `/bff/me`'s `kind` spelled the listing's way, the engine's display-name
/// fallback, and nothing about roles, groups, issuer or subject.
#[rstest]
#[case::user_with_name(
    json!({ "id": ADA_ID, "kind": "user", "issuer": "https://issuer.example.com",
            "subject": "s-1", "email": "ada@example.com", "name": "Ada Lovelace",
            "preferredUsername": "ada", "clientName": null, "clientId": null,
            "isSuperAdmin": false, "organizations": [{ "name": "my-org", "tenants": [] }] }),
    json!({ "owner": { "type": "principal", "ref": ADA_ID }, "type": "user",
            "displayName": "Ada Lovelace", "email": "ada@example.com" })
)]
#[case::user_with_username_only(
    json!({ "id": ADA_ID, "kind": "user", "issuer": "i", "subject": "s-1",
            "name": "  ", "preferredUsername": "ada" }),
    json!({ "owner": { "type": "principal", "ref": ADA_ID }, "type": "user",
            "displayName": "ada" })
)]
#[case::user_with_subject_only(
    json!({ "id": ADA_ID, "kind": "user", "issuer": "i", "subject": "s-1" }),
    json!({ "owner": { "type": "principal", "ref": ADA_ID }, "type": "user",
            "displayName": "s-1" })
)]
#[case::service_account(
    json!({ "id": ADA_ID, "kind": "service_account", "issuer": "i", "subject": "s-1",
            "clientName": "deployer", "email": "never@example.com" }),
    json!({ "owner": { "type": "principal", "ref": ADA_ID }, "type": "serviceAccount",
            "displayName": "deployer" })
)]
#[case::service_account_without_name(
    json!({ "id": ADA_ID, "kind": "service_account", "issuer": "i", "subject": "s-1" }),
    json!({ "owner": { "type": "principal", "ref": ADA_ID }, "type": "serviceAccount",
            "displayName": "s-1" })
)]
#[case::unknown_kind(
    json!({ "id": ADA_ID, "kind": "robot", "issuer": "i", "subject": "" }),
    json!({ "owner": { "type": "principal", "ref": ADA_ID }, "displayName": ADA_ID })
)]
#[tokio::test]
async fn test_me_maps_the_caller_onto_a_row(#[case] me: Value, #[case] expected: Value) {
    let engine = mock_me(me).await;

    let output = run(&engine, json!({ "me": true }))
        .await
        .expect("the caller is named");

    assert_eq!(output.payload(), &json!({ "principals": [expected] }));
}

/// `me` names one principal, so any filter beside it is refused, naming the filter.
#[rstest]
#[case::principal_type(json!({ "me": true, "type": "user" }), "type")]
#[case::ids(json!({ "me": true, "ids": [ADA_ID] }), "ids")]
#[case::display_name(json!({ "me": true, "displayName": "Ada" }), "displayName")]
#[case::email(json!({ "me": true, "email": "ada@example.com" }), "email")]
#[case::cursor(json!({ "me": true, "cursor": "abc" }), "cursor")]
#[tokio::test]
async fn test_me_refuses_other_arguments(#[case] arguments: Value, #[case] field: &str) {
    let engine = MockEngine::start().await;

    let error = run(&engine, arguments).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(error.details.as_deref(), Some(&json!({ "field": field })));
}

/// `me: false` is the listing.
#[rstest]
#[tokio::test]
async fn test_me_false_lists_the_directory() {
    let engine = mock_directory(vec![vec![mock_user(ADA_ID, "Ada", "ada@example.com")]]).await;

    let output = run(&engine, json!({ "me": false }))
        .await
        .expect("the listing succeeds");

    assert_eq!(refs(output.payload()), vec![ADA_ID.to_string()]);
}

// ---------------------------------------------------------------------------------------------
// The input rules.
// ---------------------------------------------------------------------------------------------

/// Every bound is checked before any engine call, naming the argument.
#[rstest]
#[case::no_ids(json!({ "ids": [] }), "ids")]
#[case::too_many_ids(json!({ "ids": vec![ADA_ID; 21] }), "ids")]
#[case::not_a_uuid(json!({ "ids": [ADA_ID, "ada"] }), "ids")]
#[case::blank_display_name(json!({ "displayName": "  " }), "displayName")]
#[case::blank_email(json!({ "email": "" }), "email")]
#[case::long_display_name(json!({ "displayName": "a".repeat(201) }), "displayName")]
#[case::email_of_a_service_account(
    json!({ "type": "serviceAccount", "email": "ada@example.com" }),
    "email"
)]
#[tokio::test]
async fn test_input_rules(#[case] arguments: Value, #[case] field: &str) {
    let engine = MockEngine::start().await;

    let error = run(&engine, arguments).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(error.details.as_deref(), Some(&json!({ "field": field })));
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .expect("the mock records its requests")
            .is_empty(),
        "nothing reaches the engine"
    );
}

/// A wrong `type` value fails deserialisation, which the runtime reports as `invalid_arguments`.
#[rstest]
fn test_an_unknown_type_does_not_deserialise() {
    assert!(serde_json::from_value::<ListPrincipalsInput>(json!({ "type": "group" })).is_err());
}

// ---------------------------------------------------------------------------------------------
// What is sent.
// ---------------------------------------------------------------------------------------------

/// `type`, `ids` and the narrowing `search` go to the engine; an `email` alone asks for users.
#[rstest]
#[case::nothing(json!({}), &[("limit", "20")])]
#[case::type_alone(json!({ "type": "serviceAccount" }), &[("limit", "20"), ("type", "serviceAccount")])]
#[case::email_alone(
    json!({ "email": "Ada@Example.com" }),
    &[("limit", "200"), ("search", "Ada@Example.com"), ("type", "user")]
)]
#[case::display_name_alone(json!({ "displayName": " Ada " }), &[("limit", "200"), ("search", "Ada")])]
#[case::both(
    json!({ "displayName": "Ada", "email": "ada@" }),
    &[("limit", "200"), ("search", "ada@"), ("type", "user")]
)]
#[case::ids(
    json!({ "ids": [ADA_ID.to_uppercase(), "3fa85f64-5717-4562-b3fc-2c963f66afa6"] }),
    &[("limit", "20"), ("id", "523f5c33-1a5c-7270-aa81-bbc05ab201dc,3fa85f64-5717-4562-b3fc-2c963f66afa6")]
)]
#[tokio::test]
async fn test_query_sent(#[case] arguments: Value, #[case] expected: &[(&str, &str)]) {
    let engine = mock_directory(vec![Vec::new()]).await;

    run(&engine, arguments).await.expect("the listing succeeds");

    let expected: HashMap<String, String> = expected
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    assert_eq!(sent_queries(&engine).await, vec![expected]);
}

// ---------------------------------------------------------------------------------------------
// The rows, and the local filter.
// ---------------------------------------------------------------------------------------------

/// A row is `owner`, `type`, `displayName` and `email`, in that order, each omitted when absent.
#[rstest]
#[tokio::test]
async fn test_a_row_is_the_owner_and_who_it_is() {
    let engine = mock_directory(vec![vec![
        mock_user(ADA_ID, "Ada", "ada@example.com"),
        mock_service_account("p-2", "deployer"),
        json!({ "id": "p-3" }),
    ]])
    .await;

    let output = run(&engine, json!({})).await.expect("the listing succeeds");

    assert_eq!(
        serde_json::to_string(output.payload()).expect("serialisable"),
        json!({ "principals": [
            { "owner": { "type": "principal", "ref": ADA_ID }, "type": "user",
              "displayName": "Ada", "email": "ada@example.com" },
            { "owner": { "type": "principal", "ref": "p-2" }, "type": "serviceAccount",
              "displayName": "deployer" },
            { "owner": { "type": "principal", "ref": "p-3" } }
        ] })
        .to_string()
    );
}

/// authz's `search` also hits usernames and e-mails, so a row it returns is kept only when it
/// matches every filter as given: a part, in any case, full-Unicode folded.
#[rstest]
#[tokio::test]
async fn test_local_filter_keeps_only_true_matches() {
    let engine = mock_directory(vec![vec![
        mock_user("p-1", "Ada Lovelace", "countess@example.com"),
        mock_user("p-2", "Grace Hopper", "ada.fan@example.com"),
        mock_user("p-3", "ÅDA Ström", "strom@example.com"),
        mock_service_account("p-4", "ada-deployer"),
    ]])
    .await;

    let output = run(&engine, json!({ "displayName": "åda" }))
        .await
        .expect("the listing succeeds");

    assert_eq!(refs(output.payload()), vec!["p-3".to_string()]);

    let output = run(&engine, json!({ "displayName": "ADA" }))
        .await
        .expect("the listing succeeds");

    assert_eq!(
        refs(output.payload()),
        vec!["p-1".to_string(), "p-4".to_string()]
    );
}

/// Both filters must hold, and an e-mail filter never matches a principal without one.
#[rstest]
#[tokio::test]
async fn test_filters_combine() {
    let engine = mock_directory(vec![vec![
        mock_user("p-1", "Ada Lovelace", "ada@example.com"),
        mock_user("p-2", "Ada Byron", "byron@example.com"),
        json!({ "id": "p-3", "type": "user", "displayName": "Ada" }),
    ]])
    .await;

    let output = run(
        &engine,
        json!({ "displayName": "ada", "email": "ADA@EXAMPLE" }),
    )
    .await
    .expect("the listing succeeds");

    assert_eq!(refs(output.payload()), vec!["p-1".to_string()]);
}

// ---------------------------------------------------------------------------------------------
// Paging.
// ---------------------------------------------------------------------------------------------

/// Without a local filter one engine page is one answer, and its `continue` is the cursor.
#[rstest]
#[tokio::test]
async fn test_without_a_filter_the_engine_page_is_the_answer() {
    let engine = mock_directory(vec![mock_users("a", 20), mock_users("b", 3)]).await;

    let first = run(&engine, json!({})).await.expect("the listing succeeds");
    let cursor = first.payload()["cursor"]
        .as_str()
        .expect("a cursor to the second page")
        .to_string();
    let second = run(&engine, json!({ "cursor": cursor }))
        .await
        .expect("the listing continues");

    assert_eq!(refs(first.payload()).len(), MAX_ROWS);
    assert_eq!(refs(second.payload()).len(), 3);
    assert!(second.payload().get("cursor").is_none());
}

/// With a local filter, matches spread over three engine pages come back each exactly once, in
/// order, however the answers cut the pages.
#[rstest]
#[tokio::test]
async fn test_cursor_resumes_inside_a_page() {
    // 15 matches on page one, 15 on page two, 5 on page three: answers of 20, 15.
    let page = |prefix: &str, matches: usize| {
        let mut rows = mock_users(prefix, matches);
        rows.push(mock_user(
            &format!("{prefix}-x"),
            "Someone Else",
            "x@example.org",
        ));
        rows
    };
    let engine = mock_directory(vec![page("a", 15), page("b", 15), page("c", 5)]).await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..5 {
        let mut arguments = json!({ "displayName": "user" });
        if let Some(cursor) = &cursor {
            arguments["cursor"] = json!(cursor);
        }

        let output = run(&engine, arguments).await.expect("the listing succeeds");
        seen.extend(refs(output.payload()));

        match output.payload()["cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }

    let expected: Vec<String> = ["a", "b", "c"]
        .iter()
        .zip([15, 15, 5])
        .flat_map(|(prefix, n)| (0..n).map(move |i| format!("{prefix}-{i}")))
        .collect();
    assert_eq!(seen, expected);
}

/// After five engine pages with too few matches, the answer is what matched, a cursor, and a
/// warning saying how many were checked.
#[rstest]
#[tokio::test]
async fn test_scan_stops_after_five_pages() {
    let pages: Vec<Vec<Value>> = (0..7)
        .map(|i| {
            vec![
                mock_user(&format!("p-{i}"), "Someone", "someone@example.org"),
                mock_user(&format!("q-{i}"), "Nobody", "nobody@example.org"),
            ]
        })
        .collect();
    let engine = mock_directory(pages).await;

    let output = run(&engine, json!({ "displayName": "someone" }))
        .await
        .expect("the listing succeeds");

    assert_eq!(refs(output.payload()).len(), 5);
    assert!(output.payload()["cursor"].as_str().is_some());
    let rendered = output.render(None);
    assert!(
        rendered["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning
                .as_str()
                .is_some_and(|text| text.contains("Checked 10 principals")))),
        "{rendered}"
    );
    assert_eq!(sent_queries(&engine).await.len(), 5);
}

/// A cursor continues only the listing it came from.
#[rstest]
#[tokio::test]
async fn test_cursor_bound_to_its_filters() {
    let engine = mock_directory(vec![mock_users("a", 20), mock_users("b", 3)]).await;

    let first = run(&engine, json!({})).await.expect("the listing succeeds");
    let cursor = first.payload()["cursor"]
        .as_str()
        .expect("a cursor")
        .to_string();

    let error = run(&engine, json!({ "type": "user", "cursor": cursor }))
        .await
        .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_CURSOR, Remedy::RetryAfterChange)
    );
}

// ---------------------------------------------------------------------------------------------
// The e-mail owner hint.
// ---------------------------------------------------------------------------------------------

/// A whole address that matched nobody points at the e-mail owner; a partial one, or one that
/// matched, does not.
#[rstest]
#[case::whole_address_unmatched("ada@example.com", false, true)]
#[case::partial_address_unmatched("ada@", false, false)]
#[case::whole_address_matched("ada@example.com", true, false)]
#[tokio::test]
async fn test_email_hint(#[case] email: &str, #[case] listed: bool, #[case] hinted: bool) {
    let items = if listed {
        vec![mock_user(ADA_ID, "Ada", "ada@example.com")]
    } else {
        Vec::new()
    };
    let engine = mock_directory(vec![items]).await;

    let output = run(&engine, json!({ "email": email }))
        .await
        .expect("the listing succeeds");

    let hint = output.payload().get("hint").and_then(Value::as_str);
    assert_eq!(hint.is_some(), hinted, "{}", output.payload());
    if let Some(hint) = hint {
        assert!(
            hint.contains(r#"{"type":"email","ref":"ada@example.com"}"#),
            "{hint}"
        );
    }
}

/// Nobody matching is an answer, not an error.
#[rstest]
#[tokio::test]
async fn test_no_match_is_an_empty_answer() {
    let engine = mock_directory(vec![Vec::new()]).await;

    let output = run(&engine, json!({ "displayName": "nobody" }))
        .await
        .expect("the listing succeeds");

    assert_eq!(output.payload(), &json!({ "principals": [] }));
}

// ---------------------------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------------------------

/// Every failure of the directory, worded as identity and authz, never as the catalog's contents.
#[rstest]
#[case::no_tenant_context(
    PRINCIPALS_PATH,
    400,
    "Missing required header x-mia-acl-context",
    codes::UNAUTHENTICATED,
    Remedy::Escalate
)]
#[case::other_bad_request(
    PRINCIPALS_PATH,
    400,
    "invalid query parameter 'limit'",
    codes::SERVER_DEFECT,
    Remedy::Escalate
)]
#[case::unauthenticated(
    PRINCIPALS_PATH,
    401,
    "no token",
    codes::UNAUTHENTICATED,
    Remedy::Escalate
)]
#[case::forbidden(PRINCIPALS_PATH, 403, "denied", codes::FORBIDDEN, Remedy::Escalate)]
#[case::missing_route(
    PRINCIPALS_PATH,
    404,
    "Not Found",
    codes::SERVER_DEFECT,
    Remedy::Escalate
)]
#[case::authz_conflict(PRINCIPALS_PATH, 409, "busy", codes::SERVER_DEFECT, Remedy::Escalate)]
#[case::authz_unavailable(
    PRINCIPALS_PATH,
    502,
    "the authz service is not configured",
    codes::UPSTREAM_UNAVAILABLE,
    Remedy::Retry
)]
#[case::engine_failure(
    PRINCIPALS_PATH,
    500,
    "Something went wrong",
    codes::CATALOG_UNAVAILABLE,
    Remedy::Retry
)]
#[case::me_unauthenticated(ME_PATH, 401, "no token", codes::UNAUTHENTICATED, Remedy::Escalate)]
#[case::me_missing_route(ME_PATH, 404, "Not Found", codes::SERVER_DEFECT, Remedy::Escalate)]
#[case::me_authz_unavailable(
    ME_PATH,
    502,
    "Could not reach authz service",
    codes::UPSTREAM_UNAVAILABLE,
    Remedy::Retry
)]
#[tokio::test]
async fn test_errors(
    #[case] route: &str,
    #[case] status: u16,
    #[case] message: &str,
    #[case] code: &str,
    #[case] remedy: Remedy,
) {
    let engine = mock_failing(route, status, message).await;
    let arguments = if route == ME_PATH {
        json!({ "me": true })
    } else {
        json!({})
    };

    let error = run(&engine, arguments).await.expect_err("an error");

    assert_eq!(
        (error.code, error.remedy),
        (code, remedy),
        "{}",
        error.message
    );
    if code == codes::UPSTREAM_UNAVAILABLE {
        assert!(!error.message.contains("catalog is"), "{}", error.message);
    }
}

/// A caller who may not list principals is told what is still possible.
#[rstest]
#[tokio::test]
async fn test_forbidden_points_at_what_still_works() {
    let engine = mock_failing(PRINCIPALS_PATH, 403, "denied").await;

    let error = run(&engine, json!({})).await.expect_err("an error");

    let next_step = error.next_step.unwrap_or_default();
    assert!(next_step.contains("`me: true`"), "{next_step}");
    assert!(next_step.contains("\"email\""), "{next_step}");
}
