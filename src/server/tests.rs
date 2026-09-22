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
use crate::{context::AppState, server::build_router, tools::hello::TOOL_NAME};
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
    build_router(AppState::new(mock_config), &CancellationToken::new())
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

    assert_eq!(names, vec![TOOL_NAME]);

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

    assert_eq!(names, vec![TOOL_NAME]);

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

#[rstest]
#[tokio::test]
async fn test_forwarded_header_reaches_the_tool_on_the_stateless_era(mock_router: Router) {
    let called = stateless_request(
        &mock_router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-request-id", "test-request-0001")],
    )
    .await;

    assert_eq!(
        tool_payload(&called)["requestId"],
        json!("test-request-0001"),
        "the inbound header did not survive into the tool call"
    );
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
        &[("x-request-id", "test-request-0002")],
    )
    .await;

    assert_eq!(
        tool_payload(&called)["requestId"],
        json!("test-request-0002"),
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
        &[("x-request-id", "test-request-first")],
    )
    .await;

    let second = legacy_request(
        &mock_router,
        &session_id,
        "tools/call",
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[("x-request-id", "test-request-second")],
    )
    .await;

    assert_eq!(
        tool_payload(&first)["requestId"],
        json!("test-request-first")
    );
    assert_eq!(
        tool_payload(&second)["requestId"],
        json!("test-request-second"),
        "a legacy session leaked the first request's header into the second call"
    );
}

/// The request-id layer is outermost (§6.1), so a call that arrives without one still reaches
/// the tool with a minted id rather than nothing.
#[rstest]
#[tokio::test]
async fn test_request_id_is_minted_when_absent(mock_router: Router) {
    let called = stateless_request(
        &mock_router,
        "tools/call",
        Some(TOOL_NAME),
        json!({ "name": TOOL_NAME, "arguments": {} }),
        &[],
    )
    .await;

    assert!(
        tool_payload(&called)["requestId"].is_string(),
        "no request id reached the tool"
    );
}

// ---------------------------------------------------------------------------------------------
// D11 — Host and Origin are the SDK's, and we configure them. These assert the configuration
// arrived, not that the SDK works.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_disallowed_host_is_rejected(mock_config: Config) {
    let router = build_router(AppState::new(mock_config), &CancellationToken::new());

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

    let router = build_router(AppState::new(config), &CancellationToken::new());

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
    let state = AppState::new(mock_config);
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
    let state = AppState::new(mock_config);
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
    let state = AppState::new(mock_config);
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
    let state = AppState::new(mock_config);
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

    assert_eq!(listed["result"]["tools"][0]["name"], json!(TOOL_NAME));
    assert!(listed["result"].get("nextCursor").is_none());
}
