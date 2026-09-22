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
use serde::Deserialize;

/// Default SSE keep-alive interval, in seconds — the SDK's own default, kept because it sits
/// under the usual 60 s intermediary idle timeout (§6.4).
pub static DEFAULT_SSE_KEEP_ALIVE_SECONDS: u64 = 15;

/// Returns `true`.
pub fn default_legacy_session_mode() -> bool {
    true
}

/// Returns `true`.
pub fn default_json_response() -> bool {
    true
}

/// Returns [`DEFAULT_SSE_KEEP_ALIVE_SECONDS`].
pub fn default_sse_keep_alive_seconds() -> u64 {
    DEFAULT_SSE_KEEP_ALIVE_SECONDS
}

/// The SDK transport knobs, every one of them read from configuration rather than hardcoded
/// (§6.1).
///
/// `legacy_session_mode` stays **on** because our only client today is handshake-era (D2);
/// requests that negotiate `2026-07-28` are served statelessly regardless. `json_response`
/// only *prefers* JSON — the transport falls back to SSE by itself when a handler emits a
/// notification before its result (D9).
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct TransportConfig {
    /// Whether handshake-era sessions are created and tracked.
    #[serde(default = "default_legacy_session_mode", rename = "legacySessionMode")]
    pub legacy_session_mode: bool,

    /// Whether a single JSON response is preferred over an SSE stream.
    #[serde(default = "default_json_response", rename = "jsonResponse")]
    pub json_response: bool,

    /// SSE keep-alive interval, in seconds.
    #[serde(
        default = "default_sse_keep_alive_seconds",
        rename = "sseKeepAliveSeconds"
    )]
    pub sse_keep_alive_seconds: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            legacy_session_mode: default_legacy_session_mode(),
            json_response: default_json_response(),
            sse_keep_alive_seconds: default_sse_keep_alive_seconds(),
        }
    }
}
