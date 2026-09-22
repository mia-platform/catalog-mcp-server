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

/// Default path prefix the gateway rewrites `catalog-engine` behind.
pub static DEFAULT_API_PREFIX: &str = "/api/catalog";

/// Default per-request timeout against the engine, in milliseconds (§6.4).
pub static DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Default connect timeout against the engine, in milliseconds — in-cluster (§6.4).
pub static DEFAULT_CONNECT_TIMEOUT_MS: u64 = 1_000;

/// Default retry allowance. A *policy*, not a number: the four conditions are in §8.1,
/// and a dispatched write is never retried (D20).
pub static DEFAULT_MAX_RETRIES: u8 = 1;

/// Returns [`DEFAULT_API_PREFIX`].
pub fn default_api_prefix() -> String {
    DEFAULT_API_PREFIX.to_string()
}

/// Returns [`DEFAULT_TIMEOUT_MS`].
pub fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Returns [`DEFAULT_CONNECT_TIMEOUT_MS`].
pub fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_MS
}

/// Returns [`DEFAULT_MAX_RETRIES`].
pub fn default_max_retries() -> u8 {
    DEFAULT_MAX_RETRIES
}

/// Where `catalog-engine` is reached, and how patiently (§11).
///
/// `base_url` always points at the **API gateway**, never at the engine `Service`: every
/// outbound call has to traverse `ext_authz` so the server can never do more than its caller
/// could do itself (D27, D48). Validation refuses a `base_url` that addresses the engine
/// directly.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct EngineConfig {
    /// Base URL of the API gateway in front of `catalog-engine`.
    #[serde(default, rename = "baseUrl")]
    pub base_url: String,

    /// Path prefix the gateway routes `catalog-engine` behind.
    #[serde(default = "default_api_prefix", rename = "apiPrefix")]
    pub api_prefix: String,

    /// Per-request timeout against the engine, in milliseconds.
    #[serde(default = "default_timeout_ms", rename = "timeoutMs")]
    pub timeout_ms: u64,

    /// Connect timeout against the engine, in milliseconds.
    #[serde(default = "default_connect_timeout_ms", rename = "connectTimeoutMs")]
    pub connect_timeout_ms: u64,

    /// Maximum retry attempts for a request the §8.1 policy classifies as retryable.
    #[serde(default = "default_max_retries", rename = "maxRetries")]
    pub max_retries: u8,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_prefix: default_api_prefix(),
            timeout_ms: default_timeout_ms(),
            connect_timeout_ms: default_connect_timeout_ms(),
            max_retries: default_max_retries(),
        }
    }
}
