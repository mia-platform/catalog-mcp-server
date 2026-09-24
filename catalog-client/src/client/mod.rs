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
    error::{
        BadRequestOrigin, Dispatched, ToolError, deadline_exceeded, map_status, transport_failure,
    },
    identity::CallerIdentity,
    warning::{self, EngineWarning},
};
use http::HeaderMap;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tracing::Instrument;
use url::Url;

/// The wall-clock budget bounding one whole tool call (§5.5, §6.4).
pub mod deadline;

pub use deadline::Deadline;

/// Backoff before the single permitted retry. One retry does not warrant exponential anything.
const RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// The header the engine reuses or mints, and the only actionable thing a human gets from a
/// `5xx` whose body always reads *"Something went wrong"*.
const ENGINE_REQUEST_ID_HEADER: &str = "x-request-id";

/// How many engine requests one tool call bought (§10).
pub const MCP_ENGINE_REQUESTS_TOTAL: &str = "mcp_engine_requests_total";

/// How long an engine request took (§10).
pub const MCP_ENGINE_DURATION_SECONDS: &str = "mcp_engine_duration_seconds";

/// The engine's error body: `{"status", "error", "message"}` — not RFC 7807.
#[derive(serde::Deserialize)]
struct EngineErrorBody {
    #[serde(rename = "message", default)]
    message: Option<String>,
}

/// What every engine call returns: the value, plus the warnings that came with it.
///
/// **Every engine response passes through the warning parser** (P6, D28), which is why warnings
/// are on this type rather than on the operations that happen to remember them.
#[derive(Clone, Debug)]
pub struct EngineResponse<T> {
    /// The deserialised body.
    pub value: T,

    /// Every `Warning: 299 - "…"` the response carried, in order.
    pub warnings: Vec<EngineWarning>,
}

/// The §8.1 retry policy. **A policy, not a number.**
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    /// How many retries are permitted at most (`engine.maxRetries`, default 1).
    pub max_retries: u8,

    /// One hop's connect timeout, used to decide whether the deadline has room for an attempt.
    pub connect_timeout: Duration,

    /// One hop's read timeout, used for the same decision.
    pub timeout: Duration,
}

impl RetryPolicy {
    /// Whether this failure may be retried, given everything §8.1 requires.
    ///
    /// All four conditions must hold: the request is idempotent by construction; the failure is
    /// a connect error, a read timeout or a `502`/`503`/`504` — **never a `500`**, which the
    /// engine emits for application faults and which may have committed; the deadline has room
    /// for a full further attempt; and we are below `max_retries`.
    pub fn allows(
        &self,
        idempotent: bool,
        failure: FailureKind,
        attempt: u8,
        deadline: &Deadline,
    ) -> bool {
        if !idempotent || u32::from(attempt) >= u32::from(self.max_retries) {
            return false;
        }

        if !failure.is_retryable() {
            return false;
        }

        deadline.remaining() > self.connect_timeout + self.timeout
    }
}

/// Why a request failed, at the granularity the retry policy and D20 care about.
///
/// The split between [`Self::Connect`] and [`Self::Timeout`] is not cosmetic: it is the whole of
/// what separates *"the write never left"* from *"the write may have taken effect"*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The connection was never established, so nothing was sent.
    Connect,

    /// The request went out and no answer came back in time.
    Timeout,

    /// The engine answered with this status.
    Status(u16),
}

impl FailureKind {
    /// Whether §8.1 permits a retry of this failure, ignoring every other condition.
    ///
    /// `500` is deliberately absent: the engine emits it for application faults, and an
    /// application fault may have committed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Connect | Self::Timeout => true,
            Self::Status(status) => matches!(status, 502..=504),
        }
    }

    /// Whether a **write** that met this failure may already have been applied (D20).
    ///
    /// A connect failure is the one case where the answer is no: the request never left. Every
    /// other failure leaves the outcome unknowable from here, and reporting it as a clean
    /// failure would be a lie the model would act on.
    pub fn may_have_been_applied(&self) -> bool {
        !matches!(self, Self::Connect)
    }
}

/// Whether a request changes anything, which decides how its failures are reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// A read. Idempotent by construction, and a failure is never `unknown_outcome`.
    Read,

    /// A write or a delete. Retryable only when the conflict policy has classified it so
    /// (D23), and a failure after dispatch is `unknown_outcome` (D20).
    Write {
        /// Whether the write cycle classified this call as safe to repeat.
        retryable: bool,
    },
}

impl Intent {
    /// Whether the retry policy's first condition — idempotent by construction — holds.
    fn is_idempotent(&self) -> bool {
        match self {
            Self::Read => true,
            Self::Write { retryable } => *retryable,
        }
    }

    /// How a given failure should be reported for this intent.
    fn dispatched(&self, failure: FailureKind) -> Dispatched {
        match self {
            Self::Read => Dispatched::No,
            Self::Write { .. } if failure.may_have_been_applied() => Dispatched::Yes,
            Self::Write { .. } => Dispatched::No,
        }
    }
}

/// The process-wide half of the client: one connection pool, one base URL, one policy (§8.1).
///
/// Built once at startup and cloned per request into an [`EngineClient`]. There is **no ambient
/// identity and no default ACL context** (D25): a request cannot be made from this alone.
#[derive(Clone, Debug)]
pub struct EngineClientFactory {
    http: reqwest::Client,
    base: Url,
    retry: RetryPolicy,
}

impl EngineClientFactory {
    /// Builds the process-wide client.
    ///
    /// `base_url` and `api_prefix` are joined **once**, here, with `url::Url` — never
    /// string-concatenated (D24). Both timeouts are explicit and always set: a client without
    /// them is the habit this plan names as one worth not copying from the engine.
    pub fn new(
        base_url: &str,
        api_prefix: &str,
        timeout: Duration,
        connect_timeout: Duration,
        max_retries: u8,
    ) -> anyhow::Result<Self> {
        let base = join_base(base_url, api_prefix)?;

        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .build()?;

        Ok(Self {
            http,
            base,
            retry: RetryPolicy {
                max_retries,
                connect_timeout,
                timeout,
            },
        })
    }

    /// Binds the shared client to one caller's identity and one call's deadline (D25).
    pub fn bind(&self, identity: Arc<CallerIdentity>, deadline: Deadline) -> EngineClient {
        EngineClient {
            http: self.http.clone(),
            base: self.base.clone(),
            retry: self.retry,
            identity,
            deadline,
        }
    }

    /// The joined base URL every request is resolved against.
    pub fn base_url(&self) -> &Url {
        &self.base
    }
}

/// Joins `base_url` and `api_prefix` into the one URL every path is resolved against.
///
/// The result always ends in `/`, because `Url::join` discards the last segment of a base that
/// does not — a silent way to lose the `/api/catalog` prefix on every request.
fn join_base(base_url: &str, api_prefix: &str) -> anyhow::Result<Url> {
    let mut base = Url::parse(base_url)?;

    let prefix = api_prefix.trim_matches('/');
    if !prefix.is_empty() {
        base = base.join(&format!("{prefix}/"))?;
    } else if !base.path().ends_with('/') {
        base = base.join("/")?;
    }

    Ok(base)
}

/// The request-scoped client: one caller, one deadline, one set of forwarded headers (D25).
///
/// **There is no API here that takes a `HeaderMap`.** Every request method applies the caller's
/// forwarded headers itself, so no operation — and therefore no tool — can construct a request
/// that omits the identity pair. That is how NFR-11 is met by construction rather than by
/// convention (§7.4).
#[derive(Clone, Debug)]
pub struct EngineClient {
    http: reqwest::Client,
    base: Url,
    retry: RetryPolicy,
    identity: Arc<CallerIdentity>,
    deadline: Deadline,
}

impl EngineClient {
    /// The caller this client is bound to.
    pub fn identity(&self) -> &CallerIdentity {
        &self.identity
    }

    /// The deadline bounding the whole call.
    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    /// Builds an absolute URL from path segments, percent-encoding each one.
    ///
    /// Segments are pushed rather than formatted, so nothing a caller supplies can introduce a
    /// `/`, a `?` or a `..` even if it reached here unvalidated.
    pub fn url<'a>(&self, segments: impl IntoIterator<Item = &'a str>) -> Result<Url, ToolError> {
        let mut url = self.base.clone();

        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| internal_url_error(&self.base))?;

            // The base always ends in `/`, which `url` models as a trailing empty segment.
            // Pushing onto it without dropping that segment first yields `//items`, which the
            // engine answers with a `404` that reads like a missing item.
            path.pop_if_empty();

            for segment in segments {
                path.push(segment);
            }
        }

        Ok(url)
    }

    /// Issues one `GET` and deserialises its body, applying the whole §8.1 policy.
    ///
    /// Reads are idempotent by construction, which is the first of the four retry conditions.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        operation: &'static str,
        url: Url,
        accept: &str,
    ) -> Result<EngineResponse<T>, ToolError> {
        let raw = self
            .send(operation, Request::get(url, accept), Intent::Read)
            .await?;

        self.decode(raw)
    }

    /// Issues one `PUT` and deserialises the object the engine stored.
    ///
    /// **Retried only when the conflict policy says the intent is independent of the state it
    /// lands on** (D23), and a failure after the request left is `unknown_outcome` rather than a
    /// clean failure (D20).
    pub async fn put_json<T: DeserializeOwned>(
        &self,
        operation: &'static str,
        url: Url,
        body: &Value,
        retryable: bool,
    ) -> Result<EngineResponse<T>, ToolError> {
        let raw = self
            .send(
                operation,
                Request::put(url, body.clone()),
                Intent::Write { retryable },
            )
            .await?;

        self.decode(raw)
    }

    /// Deserialises a successful response, reporting a shape mismatch as ours.
    fn decode<T: DeserializeOwned>(
        &self,
        raw: RawResponse,
    ) -> Result<EngineResponse<T>, ToolError> {
        let value = serde_json::from_slice(&raw.body).map_err(|err| {
            // A body we cannot read is our problem, not the model's: it means the engine's shape
            // and ours have diverged, which is what the contract tests exist to catch earlier.
            tracing::error!(?err, "the engine returned a body this client cannot read");

            map_status(
                406,
                BadRequestOrigin::ServerBuilt,
                Dispatched::No,
                None,
                raw.request_id.as_deref(),
            )
        })?;

        Ok(EngineResponse {
            value,
            warnings: raw.warnings,
        })
    }

    /// Issues one request, retrying once when — and only when — §8.1 permits it.
    ///
    /// One `engine.request` span per attempt (§10), carrying the **path template** rather than
    /// the interpolated one — an item name in a span label is unbounded cardinality, and the
    /// template is what an operator actually groups by.
    async fn send(
        &self,
        operation: &'static str,
        request: Request,
        intent: Intent,
    ) -> Result<RawResponse, ToolError> {
        let mut attempt: u8 = 0;

        loop {
            // `engine.timeoutMs` bounds one hop and the deadline bounds the whole call; whichever
            // is smaller wins, and a deadline with no time left fails without dialling. Nothing
            // has been sent at this point, so a write is not yet in doubt.
            if self.deadline.expired() {
                return Err(deadline_exceeded(Dispatched::No, None));
            }

            let started = std::time::Instant::now();
            let span = tracing::info_span!(
                "engine.request",
                operation,
                http.method = request.method(),
                http.status_code = tracing::field::Empty,
                duration_ms = tracing::field::Empty,
                warnings = tracing::field::Empty,
                retried = attempt > 0,
            );

            let outcome = self.attempt(&request).instrument(span.clone()).await;

            let status = match &outcome {
                Ok(_) => "2xx".to_string(),
                Err(Attempt {
                    kind: FailureKind::Status(status),
                    ..
                }) => status.to_string(),
                Err(Attempt {
                    kind: FailureKind::Connect,
                    ..
                }) => "connect_error".to_string(),
                Err(Attempt {
                    kind: FailureKind::Timeout,
                    ..
                }) => "timeout".to_string(),
            };

            span.record("http.status_code", status.as_str());
            span.record("duration_ms", started.elapsed().as_millis() as u64);
            if let Ok(response) = &outcome {
                span.record("warnings", response.warnings.len());
            }

            metrics::counter!(
                MCP_ENGINE_REQUESTS_TOTAL,
                "operation" => operation,
                "status" => status,
            )
            .increment(1);
            metrics::histogram!(MCP_ENGINE_DURATION_SECONDS, "operation" => operation)
                .record(started.elapsed().as_secs_f64());

            let (failure, request_id, message) = match outcome {
                Ok(response) => return Ok(response),
                Err(Attempt {
                    kind,
                    request_id,
                    message,
                }) => (kind, request_id, message),
            };

            if !self
                .retry
                .allows(intent.is_idempotent(), failure, attempt, &self.deadline)
            {
                return Err(self.to_tool_error(
                    failure,
                    intent.dispatched(failure),
                    request_id.as_deref(),
                    message.as_deref(),
                ));
            }

            attempt += 1;
            tracing::warn!(operation, ?failure, attempt, "retrying an engine request");
            tokio::time::sleep(jittered_backoff(attempt)).await;
        }
    }

    /// One attempt: dial, read, classify.
    async fn attempt(&self, request: &Request) -> Result<RawResponse, Attempt> {
        let builder = request
            .build(&self.http)
            // The D26 allowlist, applied here and nowhere else.
            .headers(self.identity.forwarded().clone());

        let bounded = self.deadline.bounded(builder.send());

        let response = match bounded.await {
            Err(_) => {
                return Err(Attempt {
                    kind: FailureKind::Timeout,
                    request_id: None,
                    message: None,
                });
            }
            Ok(Err(err)) if err.is_connect() => {
                return Err(Attempt {
                    kind: FailureKind::Connect,
                    request_id: None,
                    message: None,
                });
            }
            Ok(Err(err)) if err.is_timeout() || err.is_request() => {
                return Err(Attempt {
                    kind: FailureKind::Timeout,
                    request_id: None,
                    message: None,
                });
            }
            Ok(Err(err)) => {
                tracing::error!(?err, "the engine request could not be made");
                return Err(Attempt {
                    kind: FailureKind::Timeout,
                    request_id: None,
                    message: None,
                });
            }
            Ok(Ok(response)) => response,
        };

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let request_id = engine_request_id(&headers);

        let body = match self.deadline.bounded(response.bytes()).await {
            Err(_) | Ok(Err(_)) => {
                return Err(Attempt {
                    kind: FailureKind::Timeout,
                    request_id,
                    message: None,
                });
            }
            Ok(Ok(body)) => body,
        };

        if !(200..300).contains(&status) {
            return Err(Attempt {
                kind: FailureKind::Status(status),
                request_id,
                // The engine's error body is `{status, error, message}` — not RFC 7807 — and its
                // `message` is the only part worth relaying. A `500`'s is always "Something went
                // wrong", which is why the mapper replaces it rather than passing it on.
                message: serde_json::from_slice::<EngineErrorBody>(&body)
                    .ok()
                    .and_then(|body| body.message),
            });
        }

        Ok(RawResponse {
            warnings: warning::parse(&headers),
            request_id,
            body: body.to_vec(),
        })
    }

    /// Turns a classified failure into the contract's error shape.
    fn to_tool_error(
        &self,
        failure: FailureKind,
        dispatched: Dispatched,
        request_id: Option<&str>,
        engine_message: Option<&str>,
    ) -> ToolError {
        match failure {
            FailureKind::Connect => transport_failure(dispatched, request_id),
            FailureKind::Timeout => transport_failure(dispatched, request_id),
            FailureKind::Status(status) => map_status(
                status,
                BadRequestOrigin::CallerInput,
                dispatched,
                engine_message,
                request_id,
            ),
        }
    }
}

/// One outbound request, rebuildable for a retry.
///
/// `reqwest::RequestBuilder` is not `Clone` when it carries a body, so the request is described
/// rather than held — which also keeps the retry loop from accidentally retrying a half-consumed
/// body.
enum Request {
    Get { url: Url, accept: String },
    Put { url: Url, body: Value },
}

impl Request {
    /// The HTTP method, for the span field.
    fn method(&self) -> &'static str {
        match self {
            Self::Get { .. } => "GET",
            Self::Put { .. } => "PUT",
        }
    }

    /// A `GET` asking for `accept`.
    fn get(url: Url, accept: &str) -> Self {
        Self::Get {
            url,
            accept: accept.to_string(),
        }
    }

    /// A `PUT` carrying `body`.
    fn put(url: Url, body: Value) -> Self {
        Self::Put { url, body }
    }

    /// Builds the attempt.
    fn build(&self, http: &reqwest::Client) -> reqwest::RequestBuilder {
        match self {
            Self::Get { url, accept } => http
                .get(url.clone())
                .header(http::header::ACCEPT, accept.as_str()),
            Self::Put { url, body } => http
                .put(url.clone())
                .header(
                    http::header::ACCEPT,
                    crate::projection::Projection::Full.accept(),
                )
                .json(body),
        }
    }
}

/// A successful engine response, before deserialisation.
struct RawResponse {
    warnings: Vec<EngineWarning>,
    request_id: Option<String>,
    body: Vec<u8>,
}

/// One attempt's failure: classified for the retry policy, and carrying what the mapper needs.
///
/// There is deliberately no "fatal" variant. Every failure is classified, and whether it may be
/// retried is [`RetryPolicy`]'s decision alone — a second place that could decide would be a
/// second policy.
struct Attempt {
    kind: FailureKind,
    request_id: Option<String>,
    message: Option<String>,
}

/// The engine's `x-request-id`, when it sent one.
fn engine_request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ENGINE_REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 100 ms with full jitter (§8.1).
///
/// Derived from the wall clock and the attempt number rather than from a random-number
/// dependency: the only thing jitter has to achieve here is that two replicas retrying the same
/// second do not line up, and one retry does not warrant exponential anything.
fn jittered_backoff(attempt: u8) -> Duration {
    let base = RETRY_BACKOFF.as_millis() as u64;
    let nanos = u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.subsec_nanos())
            .unwrap_or_default(),
    );
    let jitter = (nanos ^ u64::from(attempt).wrapping_mul(0x9e37_79b9)) % base.max(1);

    Duration::from_millis(jitter)
}

/// A base URL that cannot have path segments is a configuration defect we cannot recover from at
/// request time; it is reported as ours, not as the model's.
fn internal_url_error(base: &Url) -> ToolError {
    tracing::error!(%base, "the configured engine base URL cannot carry a path");

    ToolError::new(
        crate::error::codes::SERVER_DEFECT,
        crate::error::Remedy::Escalate,
        "This server's engine base URL is misconfigured.",
    )
}

#[cfg(test)]
mod tests;
