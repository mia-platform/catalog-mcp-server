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
    context::AppState, handler::CatalogHandler, server::build_router, tools::hello::TOOL_NAME,
};
use axum::{
    Router,
    body::Body,
    http::{HeaderName, HeaderValue, Request, StatusCode, header},
};
use configuration::Config;
use http_body_util::BodyExt;
use rstest::{fixture, rstest};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// The one host the fixture configuration allows, so a `Host` mistake in a test reads as a
/// `403` rather than as a mystery.
const TEST_HOST: &str = "catalog-mcp.example.com";

/// Where the MCP service is mounted in the fixture configuration.
const MCP_PATH: &str = "/mcp";

/// The stateless revision: no handshake, per-request `_meta`, required `Mcp-*` headers.
const STATELESS_ERA: &str = "2026-07-28";

/// The newest handshake-era revision, which is the one our only production client speaks.
const LEGACY_ERA: &str = "2025-11-25";

/// `_meta` key carrying the per-request protocol version (required on `2026-07-28`).
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// `_meta` key carrying the per-request client capabilities (required on `2026-07-28`).
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";

/// A validated configuration, as `configuration::load` would have returned one.
#[fixture]
fn mock_config() -> Config {
    // Before any router exists, so no span callsite is ever first registered without it.
    install_span_capture();

    let mut config = Config::default();

    config.server.allowed_hosts = vec![TEST_HOST.to_string()];
    config.engine.base_url = "http://api-gateway:8080".to_string();
    config.auth.resource = "https://catalog-mcp.example.com/mcp".to_string();

    config
        .validate()
        .expect("the fixture is a valid configuration");

    config
}

/// The assembled router, over the shipped tool set.
#[fixture]
fn mock_router(mock_config: Config) -> Router {
    build_router(
        AppState::build(
            mock_config,
            crate::registry::Registry::with_shipped_tools(),
            None,
        )
        .expect("a valid state"),
        &CancellationToken::new(),
    )
}

/// One POST to the MCP endpoint, returning the status, the response headers and the body.
///
/// Requests are driven through the assembled router rather than a socket, as `catalog-engine`
/// does — which also means the `Host`, `Accept` and `Content-Type` rules the SDK enforces are
/// exercised exactly as a real client would meet them.
async fn post_mcp(
    router: &Router,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri(MCP_PATH)
        .header(header::HOST, TEST_HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");

    for (name, value) in headers {
        request = request.header(
            HeaderName::from_bytes(name.as_bytes()).expect("a well-formed test header name"),
            HeaderValue::from_str(value).expect("a well-formed test header value"),
        );
    }

    let request = request
        .body(Body::from(
            serde_json::to_vec(&body).expect("a serialisable test body"),
        ))
        .expect("a well-formed test request");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");

    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a fully buffered test body")
        .to_bytes();

    (
        status,
        response_headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The `_meta` block every stateless-era request must carry inside its `params`.
fn stateless_meta() -> Value {
    json!({
        META_PROTOCOL_VERSION: STATELESS_ERA,
        META_CLIENT_CAPABILITIES: {},
    })
}

/// Parses one JSON-RPC response, whichever framing the transport chose.
///
/// **Both framings are legal for a handled request and a client MUST support both.** The
/// transport prefers JSON only on the stateless and per-request-negotiated paths; a
/// handshake-era session is always SSE-framed, so a test that assumed JSON would be testing the
/// wrong era.
fn parse_rpc(headers: &axum::http::HeaderMap, body: &str) -> Value {
    let is_sse = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));

    let payload = if is_sse {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .find(|data| data.starts_with('{'))
            .unwrap_or_else(|| panic!("no data frame in the SSE response: {body}"))
            .to_string()
    } else {
        body.to_string()
    };

    serde_json::from_str(&payload)
        .unwrap_or_else(|err| panic!("response is not JSON-RPC: {err}: {body}"))
}

/// Sends one stateless-era request, with the `Mcp-*` headers and the `_meta` the revision
/// requires.
async fn stateless_request(
    router: &Router,
    method: &str,
    tool_name: Option<&str>,
    mut params: Value,
    extra_headers: &[(&str, &str)],
) -> Value {
    let mut headers: Vec<(&str, &str)> = vec![
        ("mcp-protocol-version", STATELESS_ERA),
        ("mcp-method", method),
    ];

    if let Some(tool_name) = tool_name {
        headers.push(("mcp-name", tool_name));
    }

    headers.extend_from_slice(extra_headers);

    params
        .as_object_mut()
        .expect("params is an object")
        .insert("_meta".to_string(), stateless_meta());

    let (status, response_headers, body) = post_mcp(
        router,
        &headers,
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "stateless {method} failed: {body}");

    parse_rpc(&response_headers, &body)
}

/// Performs the handshake and returns the session id the transport minted.
async fn legacy_handshake(router: &Router) -> String {
    let (status, headers, body) = post_mcp(
        router,
        &[],
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": LEGACY_ERA,
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1.0.0" },
            },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "initialize failed: {body}");

    let session_id = headers
        .get("mcp-session-id")
        .expect("the legacy transport mints a session id")
        .to_str()
        .expect("a UTF-8 session id")
        .to_string();

    let (status, _, _) = post_mcp(
        router,
        &[("mcp-session-id", &session_id)],
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "initialized was not accepted");

    session_id
}

/// Sends one handshake-era request on an established session.
async fn legacy_request(
    router: &Router,
    session_id: &str,
    method: &str,
    params: Value,
    extra_headers: &[(&str, &str)],
) -> Value {
    let mut headers: Vec<(&str, &str)> = vec![("mcp-session-id", session_id)];
    headers.extend_from_slice(extra_headers);

    let (status, response_headers, body) = post_mcp(
        router,
        &headers,
        json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "legacy {method} failed: {body}");

    parse_rpc(&response_headers, &body)
}

/// The single text block of a `tools/call` result, parsed back from JSON.
fn tool_payload(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a tool result carries one text block: {response}"));

    serde_json::from_str(text).expect("the tool payload is JSON")
}

// ---------------------------------------------------------------------------------------------
// The §13.2 gate: both eras, one endpoint.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_stateless_era_lists_and_calls_hello(mock_router: Router) {
    let listed = stateless_request(&mock_router, "tools/list", None, json!({}), &[]).await;

    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("a tool array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool name"))
        .collect();

    assert_eq!(
        names,
        vec![
            "describe_item",
            "get_item_schema",
            "hello",
            "list_catalog_types",
            "list_tenants",
            "search_catalog"
        ]
    );

    let called = stateless_request(
        &mock_router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[],
    )
    .await;

    assert_eq!(called["result"]["isError"], json!(false));
    assert_eq!(tool_payload(&called)["server"], json!("catalog-mcp-server"));

    // D15 — a result is one text block of compact JSON, never the same thing twice.
    assert!(
        called["result"].get("structuredContent").is_none(),
        "a result carried structuredContent while the switch is off"
    );
}

#[rstest]
#[tokio::test]
async fn test_legacy_era_lists_and_calls_hello(mock_router: Router) {
    let session_id = legacy_handshake(&mock_router).await;

    let listed = legacy_request(&mock_router, &session_id, "tools/list", json!({}), &[]).await;

    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("a tool array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool name"))
        .collect();

    assert_eq!(
        names,
        vec![
            "describe_item",
            "get_item_schema",
            "hello",
            "list_catalog_types",
            "list_tenants",
            "search_catalog"
        ]
    );

    let called = legacy_request(
        &mock_router,
        &session_id,
        "tools/call",
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[],
    )
    .await;

    assert_eq!(called["result"]["isError"], json!(false));
    assert_eq!(tool_payload(&called)["server"], json!("catalog-mcp-server"));

    // D15 — a result is one text block of compact JSON, never the same thing twice.
    assert!(
        called["result"].get("structuredContent").is_none(),
        "a result carried structuredContent while the switch is off"
    );
}

// ---------------------------------------------------------------------------------------------
// §6.2 — the identity hook. The one load-bearing integration unknown: the transport's promise to
// inject `http::request::Parts` into the request context has **no upstream integration test**,
// so these are ours. Both eras, because the injection points differ.
// ---------------------------------------------------------------------------------------------

/// The header used is `x-mia-acl-context`, and the assertion is on the tenant it decodes to —
/// because §5.5 rule 1 means **no tool ever sees a header**. What has to survive is the
/// identity, and the tenant is the observable half of it.
#[rstest]
#[tokio::test]
async fn test_forwarded_header_reaches_the_tool_on_the_stateless_era(mock_router: Router) {
    let called = stateless_request(
        &mock_router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-mia-acl-context", &acl_for("tenant-one"))],
    )
    .await;

    assert_eq!(
        tool_payload(&called)["tenant"],
        json!("my-org/tenant-one"),
        "the inbound header did not survive into the tool call"
    );
}

/// An ACL context for `tenant`, encoded as the policy layer encodes one.
fn acl_for(tenant: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    URL_SAFE_NO_PAD.encode(format!(
        r#"{{"organization":"my-org","tenant":"{tenant}"}}"#
    ))
}

#[rstest]
#[tokio::test]
async fn test_forwarded_header_reaches_the_tool_on_the_legacy_era(mock_router: Router) {
    let session_id = legacy_handshake(&mock_router).await;

    let called = legacy_request(
        &mock_router,
        &session_id,
        "tools/call",
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-mia-acl-context", &acl_for("tenant-two"))],
    )
    .await;

    assert_eq!(
        tool_payload(&called)["tenant"],
        json!("my-org/tenant-two"),
        "the inbound header did not survive into the tool call"
    );
}

/// The `Parts` ride on the **message**, not on the handler. In legacy mode one handler instance
/// serves a whole session, so a second call on the same session must see its *own* header rather
/// than the first one's — which is the property that makes D4 safe.
#[rstest]
#[tokio::test]
async fn test_each_request_sees_its_own_header_within_one_legacy_session(mock_router: Router) {
    let session_id = legacy_handshake(&mock_router).await;

    let first = legacy_request(
        &mock_router,
        &session_id,
        "tools/call",
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-mia-acl-context", &acl_for("tenant-one"))],
    )
    .await;

    let second = legacy_request(
        &mock_router,
        &session_id,
        "tools/call",
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-mia-acl-context", &acl_for("tenant-two"))],
    )
    .await;

    assert_eq!(tool_payload(&first)["tenant"], json!("my-org/tenant-one"));
    assert_eq!(
        tool_payload(&second)["tenant"],
        json!("my-org/tenant-two"),
        "a legacy session leaked the first request's identity into the second call"
    );
}

/// A call with no identity at all still reaches the tool, and the tenant it sees is `unknown` —
/// recorded and carried, never rejected (D47).
#[rstest]
#[tokio::test]
async fn test_a_call_without_an_identity_still_reaches_the_tool(mock_router: Router) {
    let called = stateless_request(
        &mock_router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[],
    )
    .await;

    assert_eq!(called["result"]["isError"], json!(false));
    assert_eq!(tool_payload(&called)["tenant"], json!("unknown/unknown"));
}

// ---------------------------------------------------------------------------------------------
// D11 — Host and Origin are the SDK's, and we configure them. These assert the configuration
// arrived, not that the SDK works.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_disallowed_host_is_rejected(mock_config: Config) {
    let router = build_router(
        AppState::build(
            mock_config,
            crate::registry::Registry::with_shipped_tools(),
            None,
        )
        .expect("a valid state"),
        &CancellationToken::new(),
    );

    let request = Request::builder()
        .method("POST")
        .uri(MCP_PATH)
        .header(header::HOST, "attacker.example.net")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", STATELESS_ERA)
        .header("mcp-method", "tools/list")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list",
                "params": { "_meta": stateless_meta() },
            }))
            .expect("a serialisable test body"),
        ))
        .expect("a well-formed test request");

    let response = router
        .oneshot(request)
        .await
        .expect("the router is infallible");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// `Origin` validation stays off while the allowlist is empty, which is what our in-cluster
/// client needs: it sends no `Origin` at all.
#[rstest]
#[tokio::test]
async fn test_origin_validation_is_off_while_the_allowlist_is_empty(mock_router: Router) {
    let (status, _, _) = post_mcp(
        &mock_router,
        &[
            ("origin", "https://anywhere.example.net"),
            ("mcp-protocol-version", STATELESS_ERA),
            ("mcp-method", "tools/list"),
        ],
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": stateless_meta() },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
}

#[rstest]
#[tokio::test]
async fn test_configured_origin_allowlist_rejects_a_foreign_origin(mock_config: Config) {
    let mut config = mock_config;
    config.server.allowed_origins = vec!["https://console.example.com".to_string()];

    let router = build_router(
        AppState::build(
            config,
            crate::registry::Registry::with_shipped_tools(),
            None,
        )
        .expect("a valid state"),
        &CancellationToken::new(),
    );

    let (status, _, _) = post_mcp(
        &router,
        &[
            ("origin", "https://attacker.example.net"),
            ("mcp-protocol-version", STATELESS_ERA),
            ("mcp-method", "tools/list"),
        ],
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": stateless_meta() },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------------------------
// The operational endpoints, unchanged from Step 0 and still outside everything request-scoped.
// ---------------------------------------------------------------------------------------------

/// Reads one operational endpoint.
async fn get(router: &Router, path: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::HOST, TEST_HOST)
                .body(Body::empty())
                .expect("a well-formed test request"),
        )
        .await
        .expect("the router is infallible");

    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a fully buffered test body")
        .to_bytes();

    (
        status,
        serde_json::from_slice(&bytes).expect("a JSON health payload"),
    )
}

/// D43 — liveness answers the engine's shape, and never depends on a dependency.
#[rstest]
#[tokio::test]
async fn test_healthz_answers_ok(mock_router: Router) {
    let (status, body) = get(&mock_router, "/-/healthz").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "catalog-mcp-server");
    assert_eq!(body["status"], "OK");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

/// D43 — liveness does **not** depend on readiness: a pod draining cleanly must not be
/// restarted for it.
#[rstest]
#[tokio::test]
async fn test_healthz_stays_ok_while_draining(mock_config: Config) {
    let state = AppState::build(
        mock_config,
        crate::registry::Registry::with_shipped_tools(),
        None,
    )
    .expect("a valid state");
    state.readiness.mark_ready();
    state.readiness.mark_draining();

    let router = build_router(state, &CancellationToken::new());
    let (status, body) = get(&router, "/-/healthz").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "OK");
}

/// The Step 0 gate: `/-/ready` reports not-ready until the startup conditions hold.
#[rstest]
#[tokio::test]
async fn test_ready_reports_not_ready_before_startup_completes(mock_router: Router) {
    let (status, body) = get(&mock_router, "/-/ready").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "KO");
}

#[rstest]
#[tokio::test]
async fn test_ready_reports_ok_once_startup_completes(mock_config: Config) {
    let state = AppState::build(
        mock_config,
        crate::registry::Registry::with_shipped_tools(),
        None,
    )
    .expect("a valid state");
    state.readiness.mark_ready();

    let router = build_router(state, &CancellationToken::new());
    let (status, body) = get(&router, "/-/ready").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "OK");
}

/// D43 — shutdown flips readiness to `503` **before** the drain begins, so the endpoint stops
/// receiving traffic while in-flight calls finish.
#[rstest]
#[tokio::test]
async fn test_ready_reports_not_ready_while_draining(mock_config: Config) {
    let state = AppState::build(
        mock_config,
        crate::registry::Registry::with_shipped_tools(),
        None,
    )
    .expect("a valid state");
    state.readiness.mark_ready();
    state.readiness.mark_draining();

    let router = build_router(state, &CancellationToken::new());
    let (status, _) = get(&router, "/-/ready").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// §6.1 — a probe must never need a token, so the operational routes answer with no identity
/// header of any kind and without the `Host` the MCP service insists on.
#[rstest]
#[tokio::test]
async fn test_operational_routes_are_not_behind_the_transport_checks(mock_config: Config) {
    let state = AppState::build(
        mock_config,
        crate::registry::Registry::with_shipped_tools(),
        None,
    )
    .expect("a valid state");
    state.readiness.mark_ready();

    let router = build_router(state, &CancellationToken::new());

    let response = router
        .oneshot(
            Request::builder()
                .uri("/-/ready")
                .header(header::HOST, "anything.example.net")
                .body(Body::empty())
                .expect("a well-formed test request"),
        )
        .await
        .expect("the router is infallible");

    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------------------------
// D13 — the cache hints, and the one thing about them that is worth stating out loud.
// ---------------------------------------------------------------------------------------------

/// `tools/list` answers `cacheScope: private` with a real `ttlMs` on a `2026-07-28` request.
///
/// This is *why* `list_tools` is hand-written: the SDK's macro emits `Public` with `ttlMs: 0`.
/// `"private"` is kept even though the set is identical for every caller (D22), because
/// `"public"` licenses an intermediary to share the response between callers even when it came
/// from an authenticated endpoint — a one-way door, for no measurable saving here.
#[rstest]
#[tokio::test]
async fn test_tools_list_carries_private_cache_hints_on_the_stateless_era(mock_router: Router) {
    let listed = stateless_request(&mock_router, "tools/list", None, json!({}), &[]).await;

    assert_eq!(listed["result"]["cacheScope"], json!("private"));
    assert_eq!(
        listed["result"]["ttlMs"],
        json!(configuration::tools::DEFAULT_TOOLS_LIST_TTL_MS)
    );
}

/// On a handshake-era request the hints are omitted, exactly as the SDK gates them.
///
/// **Note what this means:** D13's benefit does not reach today's only production client, which
/// is handshake-era (D2). Hand-writing `list_tools` still pays for itself on that path through
/// the prebuilt, byte-budgeted payload (D12) — the larger of the two reasons. Both halves are
/// tested now so neither regresses in the interval.
#[rstest]
#[tokio::test]
async fn test_tools_list_omits_cache_hints_on_the_legacy_era(mock_router: Router) {
    let session_id = legacy_handshake(&mock_router).await;
    let listed = legacy_request(&mock_router, &session_id, "tools/list", json!({}), &[]).await;

    assert!(listed["result"].get("cacheScope").is_none());
    assert!(listed["result"].get("ttlMs").is_none());
}

/// D13 — the same values are set on the `server/discover` override, whose SDK default is
/// `ttlMs: 0`. That default is the only thing wrong with it, so it is the only thing we touch.
#[rstest]
#[tokio::test]
async fn test_discover_carries_the_same_cache_hints(mock_router: Router) {
    let discovered = stateless_request(&mock_router, "server/discover", None, json!({}), &[]).await;

    assert_eq!(discovered["result"]["cacheScope"], json!("private"));
    assert_eq!(
        discovered["result"]["ttlMs"],
        json!(configuration::tools::DEFAULT_TOOLS_LIST_TTL_MS)
    );
    assert_eq!(
        discovered["result"]["capabilities"]["tools"],
        json!({}),
        "listChanged must stay absent (D5)"
    );
}

/// D12 — a `cursor` is ignored, as the SDK's own macro does: the set is not paginated, and one
/// prebuilt payload is the whole answer.
#[rstest]
#[tokio::test]
async fn test_tools_list_ignores_a_cursor(mock_router: Router) {
    let listed = stateless_request(
        &mock_router,
        "tools/list",
        None,
        json!({ "cursor": "something-a-client-invented" }),
        &[],
    )
    .await;

    assert!(
        listed["result"]["tools"]
            .as_array()
            .expect("an array")
            .iter()
            .any(|tool| tool["name"] == json!(TOOL_NAME)),
        "the whole set is listed despite the cursor"
    );
    assert_eq!(
        listed["result"]["tools"]
            .as_array()
            .expect("an array")
            .len(),
        crate::registry::Registry::with_shipped_tools()
            .tools()
            .len()
    );
    assert!(listed["result"].get("nextCursor").is_none());
}

// ---------------------------------------------------------------------------------------------
// §13.3's gate — tenant isolation, through the whole identity path.
//
// Layer → `Parts` → `MiaIdentity` → `CallerIdentity` → `EngineClient` → forwarded headers. The
// assertion is made at the far end, on what the engine actually received, because every stage in
// between is somewhere the tenant could be lost or crossed.
// ---------------------------------------------------------------------------------------------

/// A probe tool that makes one engine call, so the test can assert on what the engine saw.
///
/// It is registered into a registry of the test's own through [`AppState::build`] —
/// production has no such tool and needs no code path for one.
fn mock_engine_probe_route() -> rmcp::handler::server::router::tool::ToolRoute<CatalogHandler> {
    use crate::registry::ToolDescriptor;
    use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, Tool, ToolAnnotations};

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct ProbeInput {}

    let descriptor = ToolDescriptor::new::<ProbeInput>(
        "engine_probe",
        "Read the catalog, for tests only.",
        ToolAnnotations::new().read_only(true),
    );
    let tool: Tool = (&descriptor).into();

    rmcp::handler::server::router::tool::ToolRoute::new_dyn(
        tool,
        |context: rmcp::handler::server::tool::ToolCallContext<'_, CatalogHandler>| {
            Box::pin(async move {
                let identity =
                    std::sync::Arc::new(crate::handler::caller_identity(context.request_context()));
                let tenant = identity.tenant_key().to_string();

                let engine = context.service.state().engine_for(identity);
                let outcome = engine
                    .list_items(&catalog_client::ops::ListQuery::default())
                    .await;

                let text = serde_json::json!({
                    "tenant": tenant,
                    "ok": outcome.is_ok(),
                })
                .to_string();

                Ok(CallToolResponse::from(CallToolResult::success(vec![
                    ContentBlock::text(text),
                ])))
            })
        },
    )
}

/// Builds a server whose only tool reads the catalog, pointed at `engine_url`.
fn mock_state_against(engine_url: &str, mock_config: Config) -> AppState {
    mock_state_against_with_metrics(engine_url, mock_config, None)
}

/// As [`mock_state_against`], rendering from a given recorder.
fn mock_state_against_with_metrics(
    engine_url: &str,
    mock_config: Config,
    metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
) -> AppState {
    use crate::registry::Registry;
    use rmcp::handler::server::router::tool::ToolRouter;

    let mut config = mock_config;
    config.engine.base_url = engine_url.to_string();
    config.engine.api_prefix = "/".to_string();
    config
        .validate()
        .expect("the fixture is a valid configuration");

    AppState::build(
        config,
        Registry::new(ToolRouter::new().with_route(mock_engine_probe_route())),
        metrics,
    )
    .expect("a valid state")
}

/// Calls the probe tool on the stateless era with the given ACL context header.
async fn probe_as(router: &Router, acl_context: &str) -> Value {
    stateless_request(
        router,
        "tools/call",
        Some("engine_probe"),
        json!({ "name": "engine_probe", "arguments": {} }),
        &[("x-mia-acl-context", acl_context)],
    )
    .await
}

/// **§5.5 / D28, through the real adapter.** A tool that never looks at warnings still delivers
/// the engine's to the model, because `route_for` reads them off the call's client after the
/// tool returns — the property that makes a forgotten warning impossible rather than unlikely.
#[rstest]
#[tokio::test]
async fn test_an_engine_warning_reaches_the_model_through_the_adapter(mock_config: Config) {
    use crate::{
        registry::{Registry, route_for},
        tools::list_tenants::ListTenants,
    };
    use rmcp::handler::server::router::tool::ToolRouter;

    let engine = catalog_client::testing::MockEngine::start().await;
    engine
        .get_ok_with_warnings(
            "/bff/tenants",
            json!([]),
            &["mia-platform.eu/v1 Service is deprecated"],
        )
        .await;

    let mut config = mock_config;
    config.engine.base_url = engine.server().uri();
    config.engine.api_prefix = "/".to_string();
    config
        .validate()
        .expect("the fixture is a valid configuration");
    let state = AppState::build(
        config,
        Registry::new(ToolRouter::new().with_route(route_for(ListTenants))),
        None,
    )
    .expect("a valid state");
    let router = build_router(state, &CancellationToken::new());

    let response = stateless_request(
        &router,
        "tools/call",
        Some("list_tenants"),
        json!({ "name": "list_tenants", "arguments": {} }),
        &[],
    )
    .await;

    assert_eq!(
        tool_payload(&response)["warnings"],
        json!(["mia-platform.eu/v1 Service is deprecated"])
    );
}

/// Two callers, two tenants, two outbound contexts — and neither one is the other's.
#[rstest]
#[tokio::test]
async fn test_each_caller_reaches_the_engine_with_its_own_tenant(mock_config: Config) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let engine = catalog_client::testing::MockEngine::start().await;
    engine
        .get_ok(
            "/items",
            catalog_client::testing::mock_list_envelope(vec![], None),
        )
        .await;

    let router = build_router(
        mock_state_against(&engine.server().uri(), mock_config),
        &CancellationToken::new(),
    );

    let first = URL_SAFE_NO_PAD.encode(r#"{"organization":"my-org","tenant":"tenant-one"}"#);
    let second = URL_SAFE_NO_PAD.encode(r#"{"organization":"my-org","tenant":"tenant-two"}"#);

    assert_eq!(
        tool_payload(&probe_as(&router, &first).await)["tenant"],
        json!("my-org/tenant-one")
    );
    assert_eq!(
        tool_payload(&probe_as(&router, &second).await)["tenant"],
        json!("my-org/tenant-two")
    );

    let seen: Vec<String> = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .iter()
        .map(|request| {
            request
                .headers
                .get("x-mia-acl-context")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    assert_eq!(seen, vec![first, second], "a tenant crossed over");
}

/// The same, within **one legacy session**, where a single handler instance serves both calls.
/// This is the case D4 exists for: a cached identity would be a cross-tenant leak, not a cache.
#[rstest]
#[tokio::test]
async fn test_one_legacy_session_does_not_leak_a_tenant_between_calls(mock_config: Config) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let engine = catalog_client::testing::MockEngine::start().await;
    engine
        .get_ok(
            "/items",
            catalog_client::testing::mock_list_envelope(vec![], None),
        )
        .await;

    let router = build_router(
        mock_state_against(&engine.server().uri(), mock_config),
        &CancellationToken::new(),
    );
    let session_id = legacy_handshake(&router).await;

    let first = URL_SAFE_NO_PAD.encode(r#"{"organization":"my-org","tenant":"tenant-one"}"#);
    let second = URL_SAFE_NO_PAD.encode(r#"{"organization":"my-org","tenant":"tenant-two"}"#);

    for acl in [&first, &second] {
        legacy_request(
            &router,
            &session_id,
            "tools/call",
            json!({ "name": "engine_probe", "arguments": {} }),
            &[("x-mia-acl-context", acl)],
        )
        .await;
    }

    let seen: Vec<String> = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .iter()
        .map(|request| {
            request
                .headers
                .get("x-mia-acl-context")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    assert_eq!(
        seen,
        vec![first, second],
        "one legacy session leaked the first caller's tenant into the second call"
    );
}

/// **D47, asserted through the whole server.** A request with no identity at all still reaches
/// the tool and still produces an engine call: no `401`, no error of ours. This is the test that
/// fails if somebody re-adds a gate.
#[rstest]
#[tokio::test]
async fn test_a_request_with_no_identity_still_reaches_the_engine(mock_config: Config) {
    let engine = catalog_client::testing::MockEngine::start().await;
    engine
        .get_ok(
            "/items",
            catalog_client::testing::mock_list_envelope(vec![], None),
        )
        .await;

    let router = build_router(
        mock_state_against(&engine.server().uri(), mock_config),
        &CancellationToken::new(),
    );

    let response = stateless_request(
        &router,
        "tools/call",
        Some("engine_probe"),
        json!({ "name": "engine_probe", "arguments": {} }),
        &[],
    )
    .await;

    assert_eq!(response["result"]["isError"], json!(false));
    assert_eq!(tool_payload(&response)["tenant"], json!("unknown/unknown"));

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert_eq!(requests.len(), 1, "no engine call was made");
    assert!(requests[0].headers.get("x-mia-acl-context").is_none());
}

/// The whole D26 allowlist survives the layer, the transport and the client.
#[rstest]
#[tokio::test]
async fn test_the_full_allowlist_reaches_the_engine(mock_config: Config) {
    let engine = catalog_client::testing::MockEngine::start().await;
    engine
        .get_ok(
            "/items",
            catalog_client::testing::mock_list_envelope(vec![], None),
        )
        .await;

    let router = build_router(
        mock_state_against(&engine.server().uri(), mock_config),
        &CancellationToken::new(),
    );

    let acl = catalog_client::testing::mock_acl_context();
    stateless_request(
        &router,
        "tools/call",
        Some("engine_probe"),
        json!({ "name": "engine_probe", "arguments": {} }),
        &[
            ("x-mia-acl-context", &acl),
            (
                "x-mia-principal-id",
                catalog_client::testing::MOCK_PRINCIPAL_ID,
            ),
            ("authorization", catalog_client::testing::MOCK_BEARER),
            ("x-request-id", "test-request-0007"),
        ],
    )
    .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");
    let headers = &requests[0].headers;

    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };

    assert_eq!(value("x-mia-acl-context"), Some(acl));
    assert_eq!(
        value("x-mia-principal-id").as_deref(),
        Some(catalog_client::testing::MOCK_PRINCIPAL_ID)
    );
    assert_eq!(
        value("authorization").as_deref(),
        Some(catalog_client::testing::MOCK_BEARER)
    );
    assert_eq!(value("x-request-id").as_deref(), Some("test-request-0007"));
}

// ---------------------------------------------------------------------------------------------
// §13.4's gate — observability. `/-/metrics` reports the seven, the `mcp.request` span carries
// its fields from inside the handler, and a transport-rejected request is still counted.
// ---------------------------------------------------------------------------------------------

use crate::observability::{self, ALL_METRICS};
use std::sync::OnceLock;

/// The process-wide Prometheus recorder, installed once for the whole test binary.
///
/// `metrics` allows exactly one recorder per process, so every test that needs a rendering
/// shares this one — which also means these tests assert on *presence*, not on exact values a
/// neighbouring test could have moved.
fn shared_metrics() -> metrics_exporter_prometheus::PrometheusHandle {
    static HANDLE: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();

    HANDLE
        .get_or_init(|| observability::install().expect("the recorder installs once"))
        .clone()
}

/// A router whose state renders from the shared recorder.
fn mock_router_with_metrics(config: Config) -> Router {
    let state = AppState::build(
        config,
        crate::registry::Registry::with_shipped_tools(),
        Some(shared_metrics()),
    )
    .expect("a valid state");
    state.readiness.mark_ready();

    build_router(state, &CancellationToken::new())
}

/// All seven families render, once each has been exercised.
///
/// **They are not pre-seeded**, and that is deliberate: a counter that has never fired honestly
/// has no series, and inventing zero-valued ones would mean inventing label values — a `tool`,
/// an `outcome`, a `remedy` that nothing produced. So this test drives one of each instead: a
/// tool call, the engine call inside it, and a transport rejection. A cold scrape shows only
/// `mcp_tools_list_bytes`, which is set at startup.
#[rstest]
#[tokio::test]
async fn test_metrics_reports_the_seven_metrics(mock_config: Config) {
    let engine = catalog_client::testing::MockEngine::start().await;
    engine
        .get_ok(
            "/items",
            catalog_client::testing::mock_list_envelope(vec![], None),
        )
        .await;

    let state = mock_state_against_with_metrics(
        &engine.server().uri(),
        mock_config,
        Some(shared_metrics()),
    );
    state.readiness.mark_ready();
    let router = build_router(state, &CancellationToken::new());

    // A tool call, which makes an engine call inside it.
    stateless_request(
        &router,
        "tools/call",
        Some("engine_probe"),
        json!({ "name": "engine_probe", "arguments": {} }),
        &[],
    )
    .await;

    // A request the transport rejects before any handler runs.
    let _ = post_mcp(
        &router,
        &[
            ("mcp-protocol-version", STATELESS_ERA),
            ("mcp-method", "tools/list"),
        ],
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
    )
    .await;

    let body = scrape(&router).await;

    assert_eq!(ALL_METRICS.len(), 7);
    for metric in ALL_METRICS {
        assert!(
            body.contains(metric),
            "`{metric}` is missing from the scrape:\n{body}"
        );
    }
}

/// The headline metric of the whole rewrite is reported by the running server rather than
/// measured by hand.
#[rstest]
#[tokio::test]
async fn test_the_tools_list_size_is_reported_as_a_metric(mock_config: Config) {
    let router = mock_router_with_metrics(mock_config);

    let body = scrape(&router).await;

    assert!(
        body.lines()
            .any(|line| line.starts_with("mcp_tools_list_bytes") && !line.contains('#')),
        "mcp_tools_list_bytes carries no value:\n{body}"
    );
}

/// A tool call records its own call, duration and size.
#[rstest]
#[tokio::test]
async fn test_a_tool_call_is_counted(mock_config: Config) {
    let router = mock_router_with_metrics(mock_config);

    stateless_request(
        &router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[],
    )
    .await;

    let body = scrape(&router).await;

    assert!(
        body.contains(r#"mcp_tool_calls_total{outcome="ok",remedy="none",tool="hello"}"#)
            || body.contains(r#"tool="hello""#),
        "the call was not counted:\n{body}"
    );
}

/// **§10's stated asymmetry, asserted.** The transport rejects a malformed request before any
/// handler runs, so no `mcp.request` span exists for it — the tower layer counts it from the
/// response instead. This is the test that fails if somebody "fixes" it by parsing requests in
/// a layer.
#[rstest]
#[tokio::test]
async fn test_a_transport_rejected_request_is_still_counted(mock_config: Config) {
    let router = mock_router_with_metrics(mock_config);

    // No `_meta`, which the stateless revision requires: the transport answers `400` with
    // `-32602` and the handler never sees it.
    let (status, _, _) = post_mcp(
        &router,
        &[
            ("mcp-protocol-version", STATELESS_ERA),
            ("mcp-method", "tools/list"),
        ],
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);

    let body = scrape(&router).await;

    assert!(
        body.contains("mcp_protocol_errors_total{"),
        "a transport rejection was not counted:\n{body}"
    );
    assert!(
        body.contains(r#"era="2026-07-28""#),
        "the era label did not come from the request header:\n{body}"
    );
}

/// A `403` from the SDK's Host check carries no JSON-RPC body, and is still counted — from the
/// HTTP status, which is all the layer has to go on.
#[rstest]
#[tokio::test]
async fn test_a_host_rejection_is_counted_from_its_status(mock_config: Config) {
    let router = mock_router_with_metrics(mock_config);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(MCP_PATH)
                .header(header::HOST, "attacker.example.net")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .body(Body::from("{}"))
                .expect("a well-formed test request"),
        )
        .await
        .expect("the router is infallible");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(scrape(&router).await.contains("mcp_protocol_errors_total{"));
}

/// Reads `/-/metrics`.
async fn scrape(router: &Router) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/-/metrics")
                .header(header::HOST, TEST_HOST)
                .body(Body::empty())
                .expect("a well-formed test request"),
        )
        .await
        .expect("the router is infallible");

    String::from_utf8_lossy(
        &response
            .into_body()
            .collect()
            .await
            .expect("a fully buffered test body")
            .to_bytes(),
    )
    .into_owned()
}

/// `observability.metricsEnabled` off means no endpoint, said plainly — an empty `200` would
/// look like a server reporting nothing.
#[rstest]
#[tokio::test]
async fn test_metrics_is_absent_when_disabled(mock_config: Config) {
    let mut config = mock_config;
    config.observability.metrics_enabled = false;

    let state = AppState::build(
        config,
        crate::registry::Registry::with_shipped_tools(),
        None,
    )
    .expect("a valid state");

    let response = build_router(state, &CancellationToken::new())
        .oneshot(
            Request::builder()
                .uri("/-/metrics")
                .header(header::HOST, TEST_HOST)
                .body(Body::empty())
                .expect("a well-formed test request"),
        )
        .await
        .expect("the router is infallible");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------------
// §13.4's gate — the `mcp.request` span carries its fields **from inside the handler**.
//
// Captured as `tracing` sees it during a real request, rather than read off a log line: the
// point of §10 is *where* the span is opened. A tower layer could not record `mcp.tool` or
// `mcp.era` without reading the JSON-RPC body first — that is, without re-implementing dispatch.
// ---------------------------------------------------------------------------------------------

use std::sync::Mutex;
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

/// One captured span: its name, the fields it **declares**, and the values recorded on creation.
///
/// The two differ on purpose. A field declared `tracing::field::Empty` — `outcome`, `bytes_out`,
/// everything filled in after the work is done — is in the span's metadata from the start but
/// carries no value until `record` is called. Asserting on declarations is what checks the span
/// *shape*; asserting on values is what checks the ones known up front.
#[derive(Clone, Debug, Default)]
struct CapturedSpan {
    name: String,
    declared: Vec<String>,
    fields: std::collections::BTreeMap<String, String>,
}

/// A `tracing` layer that records every span created while it is installed.
#[derive(Clone, Default)]
struct SpanCapture(std::sync::Arc<Mutex<Vec<CapturedSpan>>>);

impl SpanCapture {
    /// Every span captured so far, from every test in the process.
    fn snapshot(&self) -> Vec<CapturedSpan> {
        self.0.lock().expect("the capture is not poisoned").clone()
    }
}

/// The process-wide span capture, installed as the **global** subscriber.
///
/// A thread-local `set_default` capture proved flaky — about one run in six, and only alongside
/// the other server tests: those hit the same span callsites on other threads with no
/// subscriber, and `tracing`'s process-wide callsite state then let this thread's subscriber miss
/// `mcp.request`. A global subscriber, installed by the fixture every server test goes through
/// and so before any router is built, takes part in every callsite's registration and removes
/// the race. Tests pick out their own spans by a marker unique to them.
static SPANS: std::sync::LazyLock<SpanCapture> = std::sync::LazyLock::new(|| {
    let capture = SpanCapture::default();
    // PANIC: test setup. A second global subscriber would be a defect in the tests themselves.
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(capture.clone()))
        .expect("no other test installs a global subscriber");
    capture
});

/// Installs [`SPANS`] once for the whole test process.
fn install_span_capture() {
    std::sync::LazyLock::force(&SPANS);
}

/// The tenant only the span test uses, so its `mcp.request` can be told from everyone else's.
const SPAN_PROBE_TENANT: &str = "span-probe";

impl<S: tracing::Subscriber> Layer<S> for SpanCapture {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: LayerContext<'_, S>,
    ) {
        let mut captured = CapturedSpan {
            name: attrs.metadata().name().to_string(),
            declared: attrs
                .metadata()
                .fields()
                .iter()
                .map(|field| field.name().to_string())
                .collect(),
            ..CapturedSpan::default()
        };

        attrs.record(
            &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                captured
                    .fields
                    .insert(field.name().to_string(), format!("{value:?}"));
            },
        );

        if let Ok(mut spans) = self.0.lock() {
            spans.push(captured);
        }
    }
}

#[rstest]
#[tokio::test]
async fn test_the_mcp_request_span_is_opened_inside_the_handler(mock_router: Router) {
    stateless_request(
        &mock_router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-mia-acl-context", &acl_for(SPAN_PROBE_TENANT))],
    )
    .await;

    let probe_tenant = format!("my-org/{SPAN_PROBE_TENANT}");
    let spans = SPANS.snapshot();
    let span = spans
        .iter()
        .find(|span| {
            span.name == "mcp.request"
                && span.fields.get("tenant").map(String::as_str) == Some(probe_tenant.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "no mcp.request span was opened; captured: {:?}",
                spans.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });

    for field in [
        "mcp.method",
        "mcp.tool",
        "mcp.era",
        "mcp.protocol_version",
        "client.name",
        "client.version",
        "tenant",
        "principal_id",
        "traceparent",
        "outcome",
        "error.code",
        "error.remedy",
        "bytes_out",
        "duration_ms",
    ] {
        assert!(
            span.declared.iter().any(|declared| declared == field),
            "the mcp.request span is missing `{field}`: {:?}",
            span.declared
        );
    }

    // The fields a tower layer could not have known without re-implementing dispatch.
    assert_eq!(
        span.fields.get("mcp.method").map(String::as_str),
        Some("\"tools/call\"")
    );
    assert_eq!(
        span.fields.get("mcp.tool").map(String::as_str),
        Some("\"hello\"")
    );
    assert_eq!(
        span.fields.get("mcp.era").map(String::as_str),
        Some("\"2026-07-28\"")
    );
    // ...and the identity the layer *did* decode, carried into it.
    assert_eq!(
        span.fields.get("tenant").map(String::as_str),
        Some(probe_tenant.as_str())
    );
}

/// The transport-only `http.request` span is opened in the layer, with the fields derivable from
/// headers alone — and **not** with the MCP-shaped ones, which is the asymmetry §4 rule 5 fixes.
#[rstest]
#[tokio::test]
async fn test_the_http_request_span_carries_transport_fields_only(mock_router: Router) {
    let _ = get(&mock_router, "/-/healthz").await;

    // Any `http.request` will do: what is asserted is the span's declared **shape**, which is the
    // same for every request, not a value this test alone sets.
    let spans = SPANS.snapshot();
    let span = spans
        .iter()
        .find(|span| span.name == "http.request")
        .expect("the layer opens an http.request span");

    for field in [
        "request_id",
        "organization",
        "tenant",
        "http.status_code",
        "duration_ms",
    ] {
        assert!(
            span.declared.iter().any(|declared| declared == field),
            "the http.request span is missing `{field}`: {:?}",
            span.declared
        );
    }

    for mcp_field in ["mcp.method", "mcp.tool", "mcp.era"] {
        assert!(
            !span.declared.iter().any(|declared| declared == mcp_field),
            "`{mcp_field}` belongs to the handler's span, not the layer's"
        );
    }
}
