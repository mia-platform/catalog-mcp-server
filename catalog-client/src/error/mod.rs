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
use serde::Serialize;
use serde_json::{Value, json};

/// The closed error-code set of §8.4, in one place.
pub mod codes;

pub use codes::ALL_CODES;

/// What the model may do about a failure. **Closed set; the whole point of the contract** (D19).
///
/// One shape for every tool, and no tool invents its own. The remedy is the field the model acts
/// on: everything else is explanation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Remedy {
    /// Transient; the same call may work.
    #[serde(rename = "retry")]
    Retry,

    /// The input was wrong; change it.
    #[serde(rename = "retry_after_change")]
    RetryAfterChange,

    /// Contention or an upstream busy; not immediately.
    #[serde(rename = "retry_later")]
    RetryLater,

    /// Nothing the model can do; tell the user.
    #[serde(rename = "escalate")]
    Escalate,

    /// **May have succeeded** — verify before retrying (D20).
    ///
    /// The category T9 asks for: a transport failure or a `5xx` *after* a write or delete was
    /// dispatched is never reported as a clean failure.
    #[serde(rename = "unknown")]
    Unknown,
}

impl Remedy {
    /// The wire value, also used as the `remedy` metric label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::RetryAfterChange => "retry_after_change",
            Self::RetryLater => "retry_later",
            Self::Escalate => "escalate",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Remedy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The one error shape every tool returns (D19, §8.4).
///
/// Rendered into the result's text block as
/// `{"error":{"code":…,"remedy":…,"message":…,"details":…,"nextStep":…}}` with `isError: true`.
/// It is a **tool** error — HTTP `200` — because anything the model could act on must be
/// something it can see and self-correct from (D18).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolError {
    /// Stable `snake_case` identifier, drawn from the closed set in [`codes`].
    pub code: &'static str,

    /// What the model may do about it.
    pub remedy: Remedy,

    /// One sentence, written for the model.
    pub message: String,

    /// Candidates, valid keys, field paths, counts — whatever makes the next attempt land.
    ///
    /// Boxed because a `Value` is large — larger still with `serde_json`'s `preserve_order`, which
    /// the workspace enables — and a `ToolError` travels in every `Result` this crate returns.
    /// Unboxed it pushes the error past clippy's `result_large_err` limit; boxed, the allocation
    /// is paid only by the errors that carry details.
    pub details: Option<Box<Value>>,

    /// The concrete next call, when there is an obvious one.
    pub next_step: Option<String>,
}

impl ToolError {
    /// Builds an error with a code from the closed set.
    pub fn new(code: &'static str, remedy: Remedy, message: impl Into<String>) -> Self {
        debug_assert!(
            ALL_CODES.contains(&code),
            "`{code}` is not in the closed error-code set of §8.4"
        );

        Self {
            code,
            remedy,
            message: message.into(),
            details: None,
            next_step: None,
        }
    }

    /// Attaches the structured detail the model needs to correct itself.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// Attaches the concrete next call.
    pub fn with_next_step(mut self, next_step: impl Into<String>) -> Self {
        self.next_step = Some(next_step.into());
        self
    }

    /// The payload the runtime puts in the result's single text block.
    pub fn to_payload(&self) -> Value {
        let mut error = serde_json::Map::new();

        error.insert("code".to_string(), json!(self.code));
        error.insert("remedy".to_string(), json!(self.remedy));
        error.insert("message".to_string(), json!(self.message));

        if let Some(details) = &self.details {
            error.insert("details".to_string(), details.as_ref().clone());
        }

        if let Some(next_step) = &self.next_step {
            error.insert("nextStep".to_string(), json!(next_step));
        }

        json!({ "error": Value::Object(error) })
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.remedy, self.message)
    }
}

impl std::error::Error for ToolError {}

/// Whether a failing request had already dispatched a write or a delete (D20).
///
/// This is what separates *"it failed"* from *"it may have succeeded"*, and it is the reason the
/// mapper takes it as an argument rather than inferring it from the HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatched {
    /// A read, or a write the client never got to send.
    No,

    /// A write or delete that reached the engine; its outcome is not knowable from here.
    Yes,
}

/// Where a `400` came from, which decides whose fault the model is told it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadRequestOrigin {
    /// The model's input: a schema or validation failure it can correct.
    CallerInput,

    /// A parameter **we** built — `rawq`, `groupBy`, `Accept`. A defect of ours, and the model
    /// is told so rather than being sent round a loop it cannot win.
    ServerBuilt,
}

/// Maps one engine outcome onto the contract (§8.4).
///
/// The engine's `500`s carry no information — the body always says *"Something went wrong"* — so
/// the message is ours, and the engine's `x-request-id` goes into `details.requestId`: the only
/// actionable thing a human gets out of a 5xx.
pub fn map_status(
    status: u16,
    origin: BadRequestOrigin,
    dispatched: Dispatched,
    engine_message: Option<&str>,
    request_id: Option<&str>,
) -> ToolError {
    // D20 — a dispatched write that fails at or after the engine is never a clean failure,
    // whatever the status says. This branch comes first for exactly that reason.
    if dispatched == Dispatched::Yes && status >= 500 {
        return dispatched_write_unknown(request_id);
    }

    let error = match status {
        400 => match origin {
            BadRequestOrigin::CallerInput => ToolError::new(
                codes::INVALID_INPUT,
                Remedy::RetryAfterChange,
                engine_message
                    .unwrap_or("The catalog rejected the request as invalid.")
                    .to_string(),
            ),
            // The engine's reason is relayed, because it is the one thing that says *which*
            // part of ours was wrong — a missing identity header on the in-cluster path reads
            // very differently from a malformed `rawq`.
            BadRequestOrigin::ServerBuilt => ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!(
                    "The catalog rejected a request this server built{}. This is not something \
                     the request can be changed to fix.",
                    engine_message
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default()
                ),
            ),
        },
        401 => ToolError::new(
            codes::UNAUTHENTICATED,
            Remedy::Escalate,
            "The caller's identity did not reach the service. This is an authentication \
             problem, not a catalog one.",
        ),
        403 => ToolError::new(
            codes::FORBIDDEN,
            Remedy::Escalate,
            "The caller is not permitted to perform this operation.",
        ),
        404 => ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            engine_message
                .unwrap_or("No such item or type in this tenant.")
                .to_string(),
        ),
        406 => ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            "The catalog rejected an `Accept` header this server sent. This is a deployment \
             defect.",
        ),
        409 => ToolError::new(
            codes::CONFLICT,
            Remedy::RetryLater,
            "The object changed while this write was in flight.",
        ),
        415 => ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            "The catalog rejected a content type this server sent. This is a deployment defect.",
        ),
        422 => ToolError::new(
            codes::UNSUPPORTED_FOR_TYPE,
            Remedy::Escalate,
            engine_message
                .unwrap_or("This kind of object does not support the requested operation.")
                .to_string(),
        ),
        501 => ToolError::new(
            codes::NOT_IMPLEMENTED,
            Remedy::Escalate,
            engine_message
                .unwrap_or("The catalog does not implement this operation.")
                .to_string(),
        ),
        502 => ToolError::new(
            codes::UPSTREAM_UNAVAILABLE,
            Remedy::Retry,
            "The authorization service is unreachable. The catalog itself may be fine.",
        ),
        status if status >= 500 => ToolError::new(
            codes::CATALOG_UNAVAILABLE,
            Remedy::Retry,
            "The catalog is unavailable. This is not the same as an empty result — nothing \
             about the catalog's contents can be concluded from it.",
        ),
        other => ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            format!("The catalog answered with an unexpected status {other}."),
        ),
    };

    attach_request_id(error, request_id)
}

/// D20 — a transport failure on a request that had already been dispatched.
pub fn dispatched_write_unknown(request_id: Option<&str>) -> ToolError {
    attach_request_id(
        ToolError::new(
            codes::UNKNOWN_OUTCOME,
            Remedy::Unknown,
            "The write was sent but its outcome is unknown: it may have taken effect. Read the \
             object back before retrying.",
        ),
        request_id,
    )
}

/// A transport failure — connect error, read timeout, broken connection — on a read.
pub fn transport_failure(dispatched: Dispatched, request_id: Option<&str>) -> ToolError {
    if dispatched == Dispatched::Yes {
        return dispatched_write_unknown(request_id);
    }

    attach_request_id(
        ToolError::new(
            codes::CATALOG_UNAVAILABLE,
            Remedy::Retry,
            "The catalog could not be reached. This is not the same as an empty result.",
        ),
        request_id,
    )
}

/// The deadline ran out (§5.5 rule 4).
pub fn deadline_exceeded(dispatched: Dispatched, request_id: Option<&str>) -> ToolError {
    if dispatched == Dispatched::Yes {
        return dispatched_write_unknown(request_id);
    }

    attach_request_id(
        ToolError::new(
            codes::DEADLINE_EXCEEDED,
            Remedy::Retry,
            "The call ran out of time before the catalog answered.",
        ),
        request_id,
    )
}

/// The caller went away while the tool was still working (§5.5 rule 5).
///
/// A tool that loops, fans out or polls `select!`s on its cancellation token and returns this.
/// The peer that would read it is usually gone; what matters is that the work stops and the
/// outcome is recorded as a cancellation rather than as a failure of the catalog.
pub fn cancelled() -> ToolError {
    ToolError::new(
        codes::CANCELLED,
        Remedy::Retry,
        "The call was cancelled before it finished.",
    )
}

/// Puts the engine's `x-request-id` in `details.requestId` — the only actionable thing a human
/// gets from a `5xx`, whose body always reads *"Something went wrong"*.
fn attach_request_id(error: ToolError, request_id: Option<&str>) -> ToolError {
    match request_id {
        Some(request_id) => {
            let mut details = error
                .details
                .clone()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();

            details.insert("requestId".to_string(), json!(request_id));

            error.with_details(Value::Object(details))
        }
        None => error,
    }
}

#[cfg(test)]
mod tests;
