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

/// Returns `true`.
pub fn default_metrics_enabled() -> bool {
    true
}

/// What the server reports about itself (§10).
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ObservabilityConfig {
    /// Whether `/-/metrics` is mounted.
    #[serde(default = "default_metrics_enabled", rename = "metricsEnabled")]
    pub metrics_enabled: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            metrics_enabled: default_metrics_enabled(),
        }
    }
}
