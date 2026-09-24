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
use crate::observability;
use axum::{
    body::Body,
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use catalog_client::identity::UNKNOWN_TENANT;
use http_body_util::BodyExt;
use std::time::Instant;

/// The header the transport reads the negotiated revision from. An absent one is `2025-03-26`
/// by the SDK's own rule, which is what the `era` label reports.
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// What the SDK assumes when no `MCP-Protocol-Version` header arrives.
const ASSUMED_ERA: &str = "2025-03-26";

/// Largest error body we will read back to extract a JSON-RPC code. A transport rejection is a
/// few hundred bytes; anything larger is not one, and buffering it would be a cost for nothing.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

/// The transport-only `http.request` span, and the protocol-error counter (§4 rule 5, §10).
///
/// **The fields here are everything derivable from headers alone**, and that is deliberate. The
/// method, the tool name and the negotiated era live *inside* the JSON-RPC body, which is the
/// SDK's to parse; reading it in a layer would re-implement dispatch. The MCP-shaped span is
/// opened inside `CatalogHandler`, where all of it is already typed.
///
/// The one thing this layer does read from a body is the JSON-RPC error code **on the way out**,
/// for `mcp_protocol_errors_total`. Protocol errors are rejected by the transport before any
/// handler runs, so no `mcp.request` span exists for them and nothing else could count them.
/// Reading a response we are already sending costs nothing and re-implements nothing.
pub async fn http_request_span(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let era = request
        .headers()
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or(ASSUMED_ERA)
        .to_string();

    let span = tracing::info_span!(
        "http.request",
        request_id = tracing::field::Empty,
        organization = UNKNOWN_TENANT,
        tenant = UNKNOWN_TENANT,
        http.status_code = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
    );

    if let Some(request_id) = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
    {
        span.record("request_id", request_id);
    }

    let response = {
        let _entered = span.enter();
        next.run(request).await
    };

    let status = response.status();
    span.record("http.status_code", status.as_u16());
    span.record("duration_ms", started.elapsed().as_millis() as u64);

    if status.is_success() {
        return response;
    }

    count_protocol_error(response, &era).await
}

/// Reads the JSON-RPC error code out of a failing response and counts it.
///
/// The body is put back exactly as it was: this observes, it does not alter.
async fn count_protocol_error(response: Response, era: &str) -> Response {
    let is_json = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));

    if !is_json {
        // A non-JSON rejection — a `403` from the Host check, say — still has an HTTP status,
        // and that is what the layer has to go on.
        observability::record_protocol_error(i64::from(response.status().as_u16()), era);

        return response;
    }

    let (parts, body) = response.into_parts();

    let Ok(collected) = body.collect().await else {
        return Response::from_parts(parts, Body::empty());
    };
    let bytes = collected.to_bytes();

    if bytes.len() <= MAX_ERROR_BODY_BYTES
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(code) = value
            .pointer("/error/code")
            .and_then(serde_json::Value::as_i64)
    {
        observability::record_protocol_error(code, era);
    } else {
        observability::record_protocol_error(i64::from(parts.status.as_u16()), era);
    }

    (parts, bytes).into_response()
}
