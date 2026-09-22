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
use crate::{context::AppState, server::build_router};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use configuration::Config;
use http_body_util::BodyExt;
use rstest::{fixture, rstest};
use tower::ServiceExt;

/// A validated configuration, as `configuration::load` would have returned one.
#[fixture]
fn mock_config() -> Config {
    let mut config = Config::default();

    config.server.allowed_hosts = vec!["catalog-mcp.example.com".to_string()];
    config.engine.base_url = "http://api-gateway:8080".to_string();
    config.auth.resource = "https://catalog-mcp.example.com/mcp".to_string();

    config
        .validate()
        .expect("the fixture is a valid configuration");

    config
}

/// Drives the assembled router without a socket, as `catalog-engine` does.
async fn get(state: AppState, path: &str) -> (StatusCode, serde_json::Value) {
    let response = build_router(state)
        .oneshot(
            Request::builder()
                .uri(path)
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
async fn test_healthz_answers_ok(mock_config: Config) {
    let state = AppState::new(mock_config);

    let (status, body) = get(state, "/-/healthz").await;

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

    let (status, body) = get(state, "/-/healthz").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "OK");
}

/// The Step 0 gate: `/-/ready` reports not-ready until the startup conditions hold.
#[rstest]
#[tokio::test]
async fn test_ready_reports_not_ready_before_startup_completes(mock_config: Config) {
    let state = AppState::new(mock_config);

    let (status, body) = get(state, "/-/ready").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "KO");
}

#[rstest]
#[tokio::test]
async fn test_ready_reports_ok_once_startup_completes(mock_config: Config) {
    let state = AppState::new(mock_config);
    state.readiness.mark_ready();

    let (status, body) = get(state, "/-/ready").await;

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

    let (status, _) = get(state, "/-/ready").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// A trailing slash is normalised, as it is in `catalog-engine`.
#[rstest]
#[tokio::test]
async fn test_operational_routes_are_not_behind_any_request_scoped_layer(mock_config: Config) {
    let state = AppState::new(mock_config);
    state.readiness.mark_ready();

    // No identity header of any kind is sent, and the probe still answers.
    let (status, _) = get(state, "/-/ready").await;

    assert_eq!(status, StatusCode::OK);
}
