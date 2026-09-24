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
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// How often a tool was called, and what happened. **The metric the A/B rests on.**
pub const MCP_TOOL_CALLS_TOTAL: &str = "mcp_tool_calls_total";

/// How long a tool call took.
pub const MCP_TOOL_DURATION_SECONDS: &str = "mcp_tool_duration_seconds";

/// How many bytes a tool answered with. **Observation, never enforcement** (D34).
pub const MCP_RESPONSE_BYTES: &str = "mcp_response_bytes";

/// How many engine requests one tool call bought.
///
/// Emitted by `catalog-client`, where the calls are made — the name is re-exported here so the
/// list below stays the one place every metric is enumerated.
pub use catalog_client::client::{MCP_ENGINE_DURATION_SECONDS, MCP_ENGINE_REQUESTS_TOTAL};

/// Protocol errors the transport rejected before any handler ran (§10's stated asymmetry).
pub const MCP_PROTOCOL_ERRORS_TOTAL: &str = "mcp_protocol_errors_total";

/// The size of the prebuilt `tools/list` payload. **The headline metric of the whole rewrite**,
/// reported by the running server rather than measured by hand.
pub const MCP_TOOLS_LIST_BYTES: &str = "mcp_tools_list_bytes";

/// Every metric this server exposes. Exactly seven, and exactly what the A/B needs: which tools
/// get called, how often they fail, what the model was told to do about it, how many bytes it
/// cost, and how many engine calls one model round trip bought.
pub const ALL_METRICS: &[&str] = &[
    MCP_TOOL_CALLS_TOTAL,
    MCP_TOOL_DURATION_SECONDS,
    MCP_RESPONSE_BYTES,
    MCP_ENGINE_REQUESTS_TOTAL,
    MCP_ENGINE_DURATION_SECONDS,
    MCP_PROTOCOL_ERRORS_TOTAL,
    MCP_TOOLS_LIST_BYTES,
];

/// What a tool call ended as, as the `outcome` label spells it (§10).
///
/// `protocol_error` is deliberately **not** here: the transport rejects those before any handler
/// runs, so no `mcp.request` span exists for them and the handler could never set it. They are
/// counted from the response by the tower layer instead. That asymmetry is written down so
/// nobody later "fixes" it by parsing requests in a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The tool answered.
    Ok,
    /// The tool returned a `ToolError`: `isError: true`, HTTP 200.
    ToolError,
    /// The client went away.
    Cancelled,
}

impl Outcome {
    /// The label value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::ToolError => "tool_error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The label value used where a metric needs a `remedy` and there is no error.
pub const REMEDY_NONE: &str = "none";

/// Installs the Prometheus recorder and returns the handle `/-/metrics` renders from.
///
/// Registering the metric families up front means `/-/metrics` reports all seven from the first
/// scrape rather than only the ones that happen to have fired — which is what makes a dashboard
/// built against it stable, and what §13.4's gate asserts.
///
/// # Errors
///
/// Fails when a recorder is already installed, which can only happen by calling this twice.
pub fn install() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(MCP_TOOL_DURATION_SECONDS.to_string()),
            &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0],
        )?
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(MCP_ENGINE_DURATION_SECONDS.to_string()),
            &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0],
        )?
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(MCP_RESPONSE_BYTES.to_string()),
            &[
                256.0,
                1_024.0,
                4_096.0,
                16_384.0,
                65_536.0,
                262_144.0,
                1_048_576.0,
            ],
        )?
        .install_recorder()?;

    describe();

    tracing::info!(metrics = ?ALL_METRICS, "metrics registered");

    Ok(handle)
}

/// Describes every metric, which is also what registers the family so an unfired metric still
/// appears in a scrape.
fn describe() {
    metrics::describe_counter!(
        MCP_TOOL_CALLS_TOTAL,
        "Tool calls, by tool, outcome and the remedy the model was given"
    );
    metrics::describe_histogram!(MCP_TOOL_DURATION_SECONDS, "Tool call duration, by tool");
    metrics::describe_histogram!(
        MCP_RESPONSE_BYTES,
        "Serialised tool response size, by tool. Measured, never enforced"
    );
    metrics::describe_counter!(
        MCP_ENGINE_REQUESTS_TOTAL,
        "Engine requests, by operation and status"
    );
    metrics::describe_histogram!(
        MCP_ENGINE_DURATION_SECONDS,
        "Engine request duration, by operation"
    );
    metrics::describe_counter!(
        MCP_PROTOCOL_ERRORS_TOTAL,
        "Requests the transport rejected before any handler ran, by JSON-RPC code and era"
    );
    metrics::describe_gauge!(
        MCP_TOOLS_LIST_BYTES,
        "Size of the prebuilt tools/list payload"
    );
}

/// Records the prebuilt payload size once at startup.
pub fn record_tools_list_bytes(bytes: usize) {
    metrics::gauge!(MCP_TOOLS_LIST_BYTES).set(bytes as f64);
}

/// Records one completed tool call.
pub fn record_tool_call(
    tool: &str,
    outcome: Outcome,
    remedy: &str,
    duration_seconds: f64,
    bytes: usize,
) {
    metrics::counter!(
        MCP_TOOL_CALLS_TOTAL,
        "tool" => tool.to_string(),
        "outcome" => outcome.as_str(),
        "remedy" => remedy.to_string(),
    )
    .increment(1);

    metrics::histogram!(MCP_TOOL_DURATION_SECONDS, "tool" => tool.to_string())
        .record(duration_seconds);

    metrics::histogram!(MCP_RESPONSE_BYTES, "tool" => tool.to_string()).record(bytes as f64);
}

/// Records one request the transport rejected before any handler ran.
pub fn record_protocol_error(code: i64, era: &str) {
    metrics::counter!(
        MCP_PROTOCOL_ERRORS_TOTAL,
        "code" => code.to_string(),
        "era" => era.to_string(),
    )
    .increment(1);
}

#[cfg(test)]
mod tests;
