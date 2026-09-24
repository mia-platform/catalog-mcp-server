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
// **This module is a published interface, not internal code** (§13.5). It is the contract the
// fifteen per-tool plans are written against, and nothing else in the core may change after it
// lands — so its surface is complete from the day it lands rather than growing tool by tool.
//
// That means parts of it are not yet called: `ProgressSink` has one future user (T4, wave 2),
// `Deadline` is consumed by tools that fan out, and `with_warnings` by the three tools that can
// produce them (T9, T12, T13). Hence the allow, on the module rather than scattered over the
// items, so that removing it later is one line.
#![allow(dead_code)]

use crate::registry::ToolDescriptor;
use catalog_client::{
    Deadline, EngineClient, EngineWarning, Remedy, TenantKey, ToolError, error::codes,
};
use rmcp::{
    model::{CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken},
    service::{Peer, RoleServer},
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// The key the runtime reserves in every rendered payload (D28).
///
/// A tool that defined it itself would silently lose either its own value or the engine's
/// warnings, so it is asserted against every registered tool rather than left to convention.
pub const WARNINGS_KEY: &str = "warnings";

/// **The tool-authoring contract. Nothing else in the core may change after it lands** (§5.5).
///
/// Five rules govern it, and they are the whole of what a tool author has to hold in mind:
///
/// 1. A tool **never** sees a header, a token, a URL, a status code or a JSON-RPC type. What it
///    is handed is a [`CallContext`], and every capability on it is opaque.
/// 2. A tool returns [`ToolOutput`] — a payload plus its warnings — and the **runtime** turns it
///    into the result. A tool cannot set `isError`; returning `Err(ToolError)` is how it fails,
///    which is the SDK's own recommendation for almost every "the tool didn't work" path.
/// 3. Argument deserialisation failures are produced by the runtime as `invalid_arguments` with
///    the serde path in `details.field` — a tool error, not a protocol error (D18).
/// 4. Exceeding the deadline is `deadline_exceeded`, except on a write already dispatched, where
///    it is `unknown_outcome` (D20). [`CallContext::deadline`] is what bounds it.
/// 5. A tool that loops, fans out or polls `select!`s on [`CallContext::cancellation`] and
///    returns `cancelled` when it fires. Single-shot tools need nothing: `deadline().bounded(..)`
///    already covers them.
pub trait Tool: Send + Sync + 'static {
    /// Deserialised tool arguments. `schemars` derives the input schema from this type.
    type Input: serde::de::DeserializeOwned + schemars::JsonSchema + Send;

    /// Static description, including the annotations. Built once at startup.
    fn descriptor() -> ToolDescriptor;

    /// Do the work.
    ///
    /// Errors are tool errors; protocol errors are not reachable from here.
    fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> impl Future<Output = Result<ToolOutput, ToolError>> + Send;
}

/// **Everything a tool is allowed to touch** (§5.5).
///
/// Built once per call by the runtime and handed out by reference. There is no way to reach an
/// identity, a header or a URL through it — which is what makes NFR-01 a property of the type
/// system rather than of anyone's discipline.
pub struct CallContext {
    engine: EngineClient,
    deadline: Deadline,
    cancellation: CancellationToken,
    progress: Option<ProgressSink>,
    tenant: TenantKey,
}

impl CallContext {
    /// Builds the context for one call.
    pub fn new(
        engine: EngineClient,
        deadline: Deadline,
        cancellation: CancellationToken,
        progress: Option<ProgressSink>,
        tenant: TenantKey,
    ) -> Self {
        Self {
            engine,
            deadline,
            cancellation,
            progress,
            tenant,
        }
    }

    /// The engine client, **already bound to the caller's identity** (D25).
    ///
    /// It exposes no method taking a `HeaderMap`, so a tool cannot construct a request that
    /// omits the identity pair (NFR-11).
    pub fn engine(&self) -> &EngineClient {
        &self.engine
    }

    /// The wall-clock budget for the whole call. Every engine call is bounded by it (D10).
    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    /// The runtime's clone of the request's cancellation token (D10, rule 5).
    ///
    /// **A tool never reaches `RequestContext` for it**, which is what keeps rule 1 true. The
    /// SDK's drop-guard is disarmed once the handler has emitted its first message, so a tool
    /// that reports progress and then loses its client is not cancelled for it — hence the
    /// cooperative `select!`.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Where to report progress, when the client asked for it (D9).
    ///
    /// `Some` only when the request carried `_meta.progressToken`. The transport switches that
    /// one response to SSE by itself the moment a notification precedes the result, so a tool
    /// needs no per-request decision and there is no second config mode.
    pub fn progress(&self) -> Option<&ProgressSink> {
        self.progress.as_ref()
    }

    /// The tenant log and metric field. Nothing is keyed by it, because nothing is stored (D31).
    pub fn tenant(&self) -> &TenantKey {
        &self.tenant
    }
}

/// Fire-and-forget progress reporting (§5.5).
///
/// **A progress report must never fail a tool call**, so a notification that cannot be
/// delivered — the peer is gone, the channel is closed — is dropped and logged rather than
/// propagated.
pub struct ProgressSink {
    peer: Peer<RoleServer>,
    token: ProgressToken,
}

impl ProgressSink {
    /// Builds a sink for a request that carried a progress token.
    pub fn new(peer: Peer<RoleServer>, token: ProgressToken) -> Self {
        Self { peer, token }
    }

    /// Reports progress. Never fails, never blocks the caller's result.
    pub async fn send(&self, progress: u32, total: Option<u32>, message: Option<&str>) {
        let mut param = ProgressNotificationParam::new(self.token.clone(), f64::from(progress));

        if let Some(total) = total {
            param = param.with_total(f64::from(total));
        }

        if let Some(message) = message {
            param = param.with_message(message);
        }

        if let Err(err) = self.peer.notify_progress(param).await {
            tracing::debug!(?err, "dropped a progress notification: the peer is gone");
        }
    }
}

/// **What a tool returns.** The runtime, not the tool, turns this into a `CallToolResult`.
///
/// It is rendered as **one** JSON object — `{…payload…, "warnings": [...]}` — merged at the top
/// level rather than nested, because a wrapper key costs bytes on every response and the model
/// has to learn it.
#[derive(Debug)]
pub struct ToolOutput {
    payload: Value,
    warnings: Option<Vec<EngineWarning>>,
}

impl ToolOutput {
    /// A payload from a tool that **cannot** produce engine warnings. The key is omitted.
    pub fn new(payload: Value) -> Self {
        Self {
            payload,
            warnings: None,
        }
    }

    /// A payload from a tool that **can** produce engine warnings (D28).
    ///
    /// The key is then **present and empty** rather than omitted, so its absence is never
    /// ambiguous: a model cannot tell "no warnings" from "this tool never warns" otherwise.
    pub fn with_warnings(payload: Value, warnings: Vec<EngineWarning>) -> Self {
        Self {
            payload,
            warnings: Some(warnings),
        }
    }

    /// The tool's own payload.
    pub fn payload(&self) -> &Value {
        &self.payload
    }

    /// The warnings, or `None` for a tool that cannot produce any.
    pub fn warnings(&self) -> Option<&[EngineWarning]> {
        self.warnings.as_deref()
    }

    /// The single JSON object the runtime serialises into the result's text block.
    pub fn render(&self) -> Value {
        let mut object = match &self.payload {
            Value::Object(object) => object.clone(),
            // A tool that returns a bare value still gets an object, because `warnings` has to
            // have somewhere to go and the model has one shape to learn.
            other => {
                let mut wrapper = Map::new();
                wrapper.insert("result".to_string(), other.clone());
                wrapper
            }
        };

        if let Some(warnings) = &self.warnings {
            object.insert(
                WARNINGS_KEY.to_string(),
                Value::Array(
                    warnings
                        .iter()
                        .map(|warning| Value::String(warning.text.clone()))
                        .collect(),
                ),
            );
        }

        Value::Object(object)
    }
}

/// Renders a successful [`ToolOutput`] as the result the model reads.
///
/// One `TextContent` block of compact JSON, and **no `structuredContent`** (D15): returning
/// every result twice doubles the metric this project exists to reduce.
pub fn success_result(output: &ToolOutput) -> CallToolResult {
    let text = serde_json::to_string(&output.render())
        .unwrap_or_else(|_| r#"{"error":"unserialisable result"}"#.to_string());

    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// Turns a serde failure into rule 3's tool error, with the field path the model needs.
pub fn invalid_arguments(err: &serde_path_to_error::Error<serde_json::Error>) -> ToolError {
    let field = err.path().to_string();

    ToolError::new(
        codes::INVALID_ARGUMENTS,
        Remedy::RetryAfterChange,
        format!("The arguments are not valid: {}", err.inner()),
    )
    .with_details(serde_json::json!({ "field": field }))
}

#[cfg(test)]
mod tests;
