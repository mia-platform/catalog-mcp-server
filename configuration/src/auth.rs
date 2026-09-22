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

/// What the server does with the caller's identity (§7.2).
///
/// `ResourceServer` is accepted by the schema — the value and this variant exist so the
/// decision is additive — but **refused by validation in v1** (D46): starting up and silently
/// behaving as `Gateway` is how a trust boundary quietly stops existing.
#[derive(Clone, Copy, Default, Deserialize, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
pub enum AuthMode {
    /// Extract, never reject: the gateway has already authenticated and authorized (D47).
    #[default]
    #[serde(rename = "gateway")]
    Gateway,

    /// Validate the bearer token ourselves. Not implemented in v1 — see P-C3.
    #[serde(rename = "resource-server")]
    ResourceServer,
}

impl AuthMode {
    /// The value as it is written in `config.json`, for error messages that name the field.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::ResourceServer => "resource-server",
        }
    }
}

/// Returns [`AuthMode::Gateway`].
pub fn default_auth_mode() -> AuthMode {
    AuthMode::Gateway
}

/// The identity posture and the canonical resource identifiers (§7.3).
///
/// The Protected Resource Metadata document is served by Envoy, not by this process, so
/// `resource` exists to match Envoy's string exactly rather than to be published from here.
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct AuthConfig {
    /// What the server does with the caller's identity.
    #[serde(default = "default_auth_mode", rename = "mode")]
    pub mode: AuthMode,

    /// The canonical resource URI, matching the `resource` value Envoy publishes.
    #[serde(default, rename = "resource")]
    pub resource: String,

    /// The URL of the Protected Resource Metadata document Envoy serves.
    #[serde(default, rename = "resourceMetadataUrl")]
    pub resource_metadata_url: String,
}
