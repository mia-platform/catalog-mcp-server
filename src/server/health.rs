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
use crate::context::AppState;
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;

/// The engine's own health shape (D43), so one dashboard reads both services.
#[derive(Serialize)]
enum Status {
    #[serde(rename = "OK")]
    Ok,
    #[serde(rename = "KO")]
    Ko,
}

impl From<bool> for Status {
    fn from(value: bool) -> Self {
        if value { Status::Ok } else { Status::Ko }
    }
}

/// `{name, status, version}` — the engine's payload, field for field (D43).
#[derive(Serialize)]
struct HealthPayload {
    name: &'static str,
    status: Status,
    version: &'static str,
}

impl HealthPayload {
    /// Renders the payload, with `503` when the reported status is `KO`.
    fn respond(healthy: bool) -> impl IntoResponse {
        let response = Json(Self {
            name: env!("CARGO_BIN_NAME"),
            status: healthy.into(),
            version: env!("CARGO_PKG_VERSION"),
        });

        if healthy {
            response.into_response()
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, response).into_response()
        }
    }
}

/// Liveness: the process is up and the runtime is not wedged (D43).
///
/// **It never calls the engine**, and it has no failing branch of its own: answering at all
/// *is* the check, because a wedged runtime never schedules this handler and the probe times
/// out. A dependency outage must not get the pod restarted, which is the whole reason liveness
/// and readiness are two endpoints and not one. Shutdown does not lower it either — a pod
/// draining cleanly must not be killed mid-drain; that is readiness' job.
async fn healthz() -> impl IntoResponse {
    HealthPayload::respond(true)
}

/// Readiness: every startup condition holds and shutdown has not begun (D43).
async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    HealthPayload::respond(state.readiness.is_ready())
}

/// The `/-/healthz` and `/-/ready` routes, to be nested under `/-`.
///
/// They are mounted **outside** the MCP service and outside the identity layer: a probe must
/// never need a token, and `/-/healthz` behind a `401` is an outage that reads as a crash-loop.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ready", get(ready))
}
