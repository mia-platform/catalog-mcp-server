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
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};

/// Renders the Prometheus text exposition.
///
/// Mounted beside the health endpoints and **outside** the identity layer: a scrape must never
/// need a token, for the same reason a probe must not.
async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    match state.metrics.as_ref() {
        Some(handle) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4",
            )],
            handle.render(),
        )
            .into_response(),
        // `observability.metricsEnabled` is off, so there is nothing to render. A `404` says
        // that plainly; an empty `200` would look like a server reporting nothing.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The `/-/metrics` route, to be nested under `/-`.
pub fn routes() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}
