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

/// Default timeout for the readiness engine probe, in milliseconds (D43).
pub static DEFAULT_ENGINE_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Returns `true`.
pub fn default_readiness_checks_engine() -> bool {
    true
}

/// Returns [`DEFAULT_ENGINE_PROBE_TIMEOUT_MS`].
pub fn default_engine_probe_timeout_ms() -> u64 {
    DEFAULT_ENGINE_PROBE_TIMEOUT_MS
}

/// What `/-/ready` checks (D43).
///
/// `/-/healthz` takes nothing from here: liveness never calls the engine, because a dependency
/// outage must not get the pod restarted.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct HealthConfig {
    /// Whether readiness also probes the engine.
    #[serde(
        default = "default_readiness_checks_engine",
        rename = "readinessChecksEngine"
    )]
    pub readiness_checks_engine: bool,

    /// Timeout for that probe, in milliseconds.
    #[serde(
        default = "default_engine_probe_timeout_ms",
        rename = "engineProbeTimeoutMs"
    )]
    pub engine_probe_timeout_ms: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            readiness_checks_engine: default_readiness_checks_engine(),
            engine_probe_timeout_ms: default_engine_probe_timeout_ms(),
        }
    }
}
