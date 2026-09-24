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
    tools::list_tenants::{ListTenants, ListTenantsInput, TOOL_NAME},
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, TenantKey, ToolError,
    error::codes,
    testing::{MockEngine, mock_acl_context, mock_error_body},
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path},
};

/// A tenant as the authz service describes one: `name` is the slug, `title` the display name.
fn mock_tenant(slug: &str, title: &str) -> Value {
    json!({ "name": slug, "organization": "my-org", "title": title })
}

/// A call context pointed at `engine`, carrying `acl` as the forwarded context.
fn mock_context(engine: &MockEngine, acl: Option<&str>) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        acl,
        None,
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

/// Runs the tool against a mock answering `GET /bff/tenants` with `body`.
async fn call_with(body: Value, acl: Option<&str>) -> Result<ToolOutput, ToolError> {
    let engine = MockEngine::start().await;
    engine.get_ok("/bff/tenants", body).await;

    ListTenants
        .call(&mock_context(&engine, acl), ListTenantsInput {})
        .await
}

/// Runs the tool against a mock answering `GET /bff/tenants` with `status`.
async fn call_failing(status: u16, message: &str) -> ToolError {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path("/bff/tenants"))
        .respond_with(ResponseTemplate::new(status).set_body_json(mock_error_body(status, message)))
        .mount(engine.server())
        .await;

    ListTenants
        .call(
            &mock_context(&engine, Some(&mock_acl_context())),
            ListTenantsInput {},
        )
        .await
        .expect_err("a failing engine is an error")
}

// ---------------------------------------------------------------------------------------------
// The surface.
// ---------------------------------------------------------------------------------------------

/// T11-D1 — no parameters, deliberately. Offering a filter would invite the model to believe it
/// can widen its own scope.
#[rstest]
fn test_the_tool_takes_no_arguments() {
    let schema = serde_json::to_string(&ListTenants::descriptor().input_schema)
        .expect("a serialisable schema");

    assert_eq!(schema, r#"{"additionalProperties":false,"type":"object"}"#);
}

#[rstest]
fn test_the_descriptor_is_read_only_and_named() {
    let descriptor = ListTenants::descriptor();

    assert_eq!(descriptor.name, TOOL_NAME);
    assert_eq!(descriptor.annotations.read_only_hint, Some(true));
    assert_eq!(descriptor.annotations.destructive_hint, None);
}

// ---------------------------------------------------------------------------------------------
// The output, and `current`.
// ---------------------------------------------------------------------------------------------

/// The engine's `name` is the slug and its `title` the display name, so the tool reports them
/// as `id` and `name` — which is what makes `current` comparable to an entry in the list.
#[rstest]
#[tokio::test]
async fn test_a_tenant_is_projected_to_three_fields() {
    let output = call_with(
        json!([mock_tenant("my-tenant", "My Tenant")]),
        Some(&mock_acl_context()),
    )
    .await
    .expect("the listing succeeds");

    assert_eq!(
        output.render()["tenants"],
        json!([{ "id": "my-tenant", "name": "My Tenant", "organization": "my-org" }])
    );
}

/// T11-D2 — `current` comes from the forwarded ACL context, never from a second call.
#[rstest]
#[tokio::test]
async fn test_current_comes_from_the_forwarded_context() {
    let output = call_with(
        json!([mock_tenant("my-tenant", "My Tenant")]),
        Some(&mock_acl_context()),
    )
    .await
    .expect("the listing succeeds");

    assert_eq!(output.render()["current"], json!("my-tenant"));
}

/// A context without a `tenantName` still yields `current`: the slug is what it is derived from.
#[rstest]
#[tokio::test]
async fn test_current_does_not_need_a_tenant_name() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let acl = URL_SAFE_NO_PAD.encode(r#"{"organization":"my-org","tenant":"tenant-two"}"#);

    let output = call_with(json!([mock_tenant("tenant-two", "Tenant Two")]), Some(&acl))
        .await
        .expect("the listing succeeds");

    assert_eq!(output.render()["current"], json!("tenant-two"));
}

/// **Omitted, never null.** Under D47 the identity layer extracts rather than rejects, so an
/// undecodable context is a reachable state — and the tool must not invent a value for it.
#[rstest]
#[tokio::test]
async fn test_current_is_omitted_when_the_context_did_not_decode() {
    let output = call_with(json!([mock_tenant("my-tenant", "My Tenant")]), None)
        .await
        .expect("the listing succeeds");

    let rendered = output.render();

    assert!(rendered.get("current").is_none(), "{rendered}");
    assert!(rendered["tenants"].as_array().is_some());
}

#[rstest]
#[tokio::test]
async fn test_a_malformed_context_also_omits_current() {
    let output = call_with(
        json!([mock_tenant("my-tenant", "My Tenant")]),
        Some("!!!not-base64!!!"),
    )
    .await
    .expect("the listing succeeds");

    assert!(output.render().get("current").is_none());
}

/// T11-D5 — an empty list is a real, successful answer, and materially different from a `401`.
#[rstest]
#[tokio::test]
async fn test_an_empty_list_is_not_an_error() {
    let output = call_with(json!([]), Some(&mock_acl_context()))
        .await
        .expect("an empty list is a successful answer");

    assert_eq!(output.render()["tenants"], json!([]));
    assert_eq!(output.render()["current"], json!("my-tenant"));
}

/// The response is a **bare array**, not a `List` envelope — so nothing here paginates.
#[rstest]
#[tokio::test]
async fn test_the_bare_array_response_is_read_directly() {
    let output = call_with(
        json!([
            mock_tenant("tenant-one", "Tenant One"),
            mock_tenant("tenant-two", "Tenant Two"),
        ]),
        Some(&mock_acl_context()),
    )
    .await
    .expect("the listing succeeds");

    assert_eq!(
        output.render()["tenants"]
            .as_array()
            .expect("an array")
            .len(),
        2
    );
}

/// This tool cannot produce engine warnings, so the key is omitted rather than empty (D28).
#[rstest]
#[tokio::test]
async fn test_no_warnings_key_is_emitted() {
    let output = call_with(json!([]), Some(&mock_acl_context()))
        .await
        .expect("the listing succeeds");

    assert!(output.render().get("warnings").is_none());
    assert_eq!(output.warnings(), None);
}

// ---------------------------------------------------------------------------------------------
// §6 of the T11 plan — every row, asserting `code` **and** `remedy`.
// ---------------------------------------------------------------------------------------------

/// **T11-D3.** A `401` is an identity failure, never a catalog failure — and the message must
/// not send the model hunting for a catalog problem that does not exist.
///
/// **T11 §7 asks for "a test asserts the string does not contain `catalog`". That literal test
/// cannot pass, and should not.** Core §8.4 fixes the wording for this row as *"This is an
/// authentication problem, not a catalog one"* — which contains the word precisely in order to
/// rule the catalog out. Asserting its absence would force a weaker message. So the assertion
/// here is on the intent the two documents share: the failure is attributed to identity, and
/// **not** attributed to the catalog.
#[rstest]
#[tokio::test]
async fn test_a_401_is_an_identity_failure_and_says_so() {
    let error = call_failing(401, "unauthorized").await;
    let message = error.message.to_lowercase();

    assert_eq!(error.code, codes::UNAUTHENTICATED);
    assert_eq!(error.remedy, Remedy::Escalate);

    assert!(
        message.contains("identity") && message.contains("authentication"),
        "the 401 must be attributed to identity: {}",
        error.message
    );

    for blaming in [
        "the catalog is",
        "catalog is unavailable",
        "catalog error",
        "catalog problem.",
    ] {
        assert!(
            !message.contains(blaming),
            "the 401 message blames the catalog (`{blaming}`): {}",
            error.message
        );
    }
}

/// **T11-D4.** A `502` says *authz*, not *the catalog*: every other tool may be working, and a
/// model told "the catalog is unavailable" would stop doing things it could still do.
#[rstest]
#[tokio::test]
async fn test_a_502_names_authz_rather_than_the_catalog() {
    let error = call_failing(502, "bad gateway").await;

    assert_eq!(error.code, codes::UPSTREAM_UNAVAILABLE);
    assert_eq!(error.remedy, Remedy::Retry);
    assert!(error.message.contains("authorization service"));
    assert!(!error.message.to_lowercase().contains("the catalog is"));
}

/// A `5XX` from the engine itself is a catalog outage, and is distinguishable from the `502`
/// above in both code and wording.
#[rstest]
#[tokio::test]
async fn test_a_500_is_a_catalog_outage_and_is_distinguishable_from_authz() {
    let error = call_failing(500, "Something went wrong").await;

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert_eq!(error.remedy, Remedy::Retry);
    assert_ne!(error.code, codes::UPSTREAM_UNAVAILABLE);
    assert!(error.message.contains("catalog"));
}

/// Rule 4 — the deadline is the runtime's, and a read that runs out of it is retryable.
#[rstest]
#[tokio::test(start_paused = true)]
async fn test_an_exhausted_deadline_is_reported_as_one() {
    let engine = MockEngine::start().await;
    engine.get_ok("/bff/tenants", json!([])).await;

    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        None,
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
        Deadline::starting_now(Duration::from_secs(1)),
    );

    let context = CallContext::new(
        client,
        Deadline::starting_now(Duration::from_secs(1)),
        CancellationToken::new(),
        None,
        TenantKey::default(),
    );

    tokio::time::advance(Duration::from_secs(5)).await;

    let error = ListTenants
        .call(&context, ListTenantsInput {})
        .await
        .expect_err("an exhausted deadline is an error");

    assert_eq!(error.code, codes::DEADLINE_EXCEEDED);
    assert_eq!(error.remedy, Remedy::Retry);
}

/// D26 — the tool makes no request of its own: the client forwards the allowlist, and this is
/// the one route where no policy regenerates it, so what the engine sees is what we sent.
#[rstest]
#[tokio::test]
async fn test_the_forwarded_identity_reaches_the_tenants_endpoint() {
    let engine = MockEngine::start().await;
    engine.get_ok("/bff/tenants", json!([])).await;

    let acl = mock_acl_context();
    let _ = ListTenants
        .call(&mock_context(&engine, Some(&acl)), ListTenantsInput {})
        .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert_eq!(requests.len(), 1, "T11 makes exactly one engine call");
    assert_eq!(requests[0].url.path(), "/bff/tenants");
    assert!(
        requests[0].url.query().is_none(),
        "the endpoint takes no parameters"
    );
    assert_eq!(
        requests[0]
            .headers
            .get("x-mia-acl-context")
            .and_then(|value| value.to_str().ok()),
        Some(acl.as_str())
    );
    assert!(requests[0].headers.get("authorization").is_some());
}
