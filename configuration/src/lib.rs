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
// Configuration for the Catalog MCP server: the structs, their defaults, and the validation
// rules that refuse to start rather than surprise somebody at request time (§11, D40).
//
// The file is JSON at `$CONFIGURATION_FOLDER/config.json`, per-field `camelCase` renames and no
// blanket `rename_all` — engine convention. It holds no secret: the only sensitive value in this
// server is the caller's bearer token, which is per-request and never configured (D45).
//
// NOTE: the Apache-2.0 header above (D44) is a `/** */` block, which Rust parses as an *outer*
// doc comment; a `//!` inner doc comment cannot follow one. Module prose therefore uses plain
// comments here and doc comments on the items themselves.

use crate::{
    auth::{AuthConfig, AuthMode},
    engine::EngineConfig,
    error::{
        ConfigError, FIELD_AUTH_MODE, FIELD_AUTH_RESOURCE, FIELD_ENGINE_BASE_URL,
        FIELD_SERVER_ALLOWED_HOSTS, FIELD_TOOLS_CALL_DEADLINE_SECONDS,
    },
    health::HealthConfig,
    observability::ObservabilityConfig,
    response::ResponseConfig,
    server::ServerConfig,
    tools::ToolsConfig,
    transport::TransportConfig,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The identity posture and the canonical resource identifiers (§7.2, §7.3).
pub mod auth;
/// Where `catalog-engine` is reached, and how patiently (§6.4, D27, D48).
pub mod engine;
/// Why a configuration was refused, and which field to change (D40).
pub mod error;
/// What `/-/ready` checks (D43).
pub mod health;
/// What the server reports about itself (§10).
pub mod observability;
/// How a tool result is rendered (D15).
pub mod response;
/// How the HTTP listener is bound and what the transport accepts (D11).
pub mod server;
/// What bounds a tool call and what the tool set advertises (§6.4, D13).
pub mod tools;
/// The SDK transport knobs (§6.1, D2, D9).
pub mod transport;

/// Name of the configuration file inside the configuration folder.
pub const CONFIG_FILE_NAME: &str = "config.json";

/// The first DNS label of the `catalog-engine` `Service`, which `engine.baseUrl` must never
/// address (D27, D48): calling it directly skips `ext_authz` and turns this server into an
/// authorization bypass.
pub const ENGINE_SERVICE_LABEL: &str = "catalog-engine";

/// Milliseconds in one second, for comparing `tools.callDeadlineSeconds` against the engine
/// timeouts.
const MILLIS_PER_SECOND: u64 = 1_000;

/// The whole service configuration (§11).
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Config {
    /// How the HTTP listener is bound and what the transport accepts.
    #[serde(default, rename = "server")]
    pub server: ServerConfig,

    /// Where `catalog-engine` is reached, and how patiently.
    #[serde(default, rename = "engine")]
    pub engine: EngineConfig,

    /// The identity posture and the canonical resource identifiers.
    #[serde(default, rename = "auth")]
    pub auth: AuthConfig,

    /// What bounds a tool call and what the tool set advertises.
    #[serde(default, rename = "tools")]
    pub tools: ToolsConfig,

    /// How a tool result is rendered.
    #[serde(default, rename = "response")]
    pub response: ResponseConfig,

    /// The SDK transport knobs.
    #[serde(default, rename = "transport")]
    pub transport: TransportConfig,

    /// What `/-/ready` checks.
    #[serde(default, rename = "health")]
    pub health: HealthConfig,

    /// What the server reports about itself.
    #[serde(default, rename = "observability")]
    pub observability: ObservabilityConfig,
}

impl Config {
    /// Refuses a configuration that would fail later, or fail silently (§11, D11, D40, D46).
    ///
    /// Runs after deserialisation and **before the listener binds**. Each refusal names the
    /// field an operator has to change, because the message is the only thing they see.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_allowed_hosts()?;
        self.validate_auth()?;
        self.validate_engine_base_url()?;
        self.validate_call_deadline()?;

        Ok(())
    }

    /// D11: an empty `Host` allowlist leaves the SDK on its loopback-only default, which is a
    /// total outage that reads as a routing bug. Refuse it, naming the symptom.
    fn validate_allowed_hosts(&self) -> Result<(), ConfigError> {
        if self.server.allowed_hosts.is_empty() {
            return Err(ConfigError::invalid(
                FIELD_SERVER_ALLOWED_HOSTS,
                "must not be empty: the MCP transport validates the inbound `Host` header \
                 against this list and defaults to loopback only, so every request behind an \
                 ingress would be answered `403 Forbidden: Host header is not allowed`. Set the \
                 hostnames this deployment is reached at, or [\"localhost\", \"127.0.0.1\", \
                 \"::1\"] to keep it loopback-only deliberately",
            ));
        }

        Ok(())
    }

    /// D46: a config value for an unimplemented mode is a startup failure, not a surprise at
    /// request time. §7.3: `auth.resource` must match Envoy's published string exactly, so it
    /// is checked for canonical form.
    fn validate_auth(&self) -> Result<(), ConfigError> {
        if self.auth.mode == AuthMode::ResourceServer {
            return Err(ConfigError::invalid(
                FIELD_AUTH_MODE,
                "`resource-server` is not implemented; see P-C3. Use `gateway`",
            ));
        }

        if let Err(reason) = canonical_uri_violation(&self.auth.resource) {
            return Err(ConfigError::invalid(FIELD_AUTH_RESOURCE, reason));
        }

        Ok(())
    }

    /// D27 and D48: every outbound call traverses the gateway, so `ext_authz` evaluates the
    /// caller's own roles against the operation we are about to perform. Addressing the engine
    /// `Service` directly would skip it entirely — invisible in testing, because everything
    /// keeps working and only the wrong people can suddenly do things.
    fn validate_engine_base_url(&self) -> Result<(), ConfigError> {
        let url = url::Url::parse(&self.engine.base_url).map_err(|err| {
            ConfigError::invalid(
                FIELD_ENGINE_BASE_URL,
                format!(
                    "must be an absolute URL addressing the API gateway, for example \
                     `http://api-gateway:8080`: {err}"
                ),
            )
        })?;

        let Some(host) = url.host_str() else {
            return Err(ConfigError::invalid(
                FIELD_ENGINE_BASE_URL,
                "must be an absolute URL with a host, for example `http://api-gateway:8080`",
            ));
        };

        let first_label = host.split('.').next().unwrap_or(host);
        if first_label.eq_ignore_ascii_case(ENGINE_SERVICE_LABEL) {
            return Err(ConfigError::invalid(
                FIELD_ENGINE_BASE_URL,
                format!(
                    "must address the API gateway, not the `{ENGINE_SERVICE_LABEL}` Service: \
                     calling the engine directly skips `ext_authz`, so this server would be \
                     able to do more than its caller could do itself"
                ),
            ));
        }

        Ok(())
    }

    /// §11: a per-call deadline shorter than one engine hop makes every call time out at the
    /// wrong layer, reported as ours rather than as the engine's.
    fn validate_call_deadline(&self) -> Result<(), ConfigError> {
        let deadline_ms = self
            .tools
            .call_deadline_seconds
            .saturating_mul(MILLIS_PER_SECOND);
        let one_hop_ms = self
            .engine
            .timeout_ms
            .saturating_add(self.engine.connect_timeout_ms);

        if deadline_ms < one_hop_ms {
            return Err(ConfigError::invalid(
                FIELD_TOOLS_CALL_DEADLINE_SECONDS,
                format!(
                    "is {deadline_ms} ms, below `engine.timeoutMs` + `engine.connectTimeoutMs` \
                     ({one_hop_ms} ms): every call would time out at the wrong layer"
                ),
            ));
        }

        Ok(())
    }
}

/// Returns why `value` is not a canonical URI, or `Ok(())` when it is.
///
/// Canonical here means what §7.3 asks for: a scheme is present, there is no fragment, and
/// there is no trailing slash. The raw string is checked for the trailing slash rather than the
/// parsed path, because `url` normalises an empty path to `/` and would hide the difference.
fn canonical_uri_violation(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(
            "must be set to the canonical resource URI Envoy publishes, for example \
             `https://catalog-mcp.example.com/mcp`"
                .to_string(),
        );
    }

    if value.contains('#') {
        return Err(format!("must not carry a fragment: `{value}`"));
    }

    if value.ends_with('/') {
        return Err(format!("must not end with a trailing slash: `{value}`"));
    }

    let url = url::Url::parse(value)
        .map_err(|err| format!("must be a canonical absolute URI: `{value}`: {err}"))?;

    if url.cannot_be_a_base() {
        return Err(format!(
            "must be a canonical absolute URI with a host: `{value}`"
        ));
    }

    Ok(())
}

/// Reads, parses and validates `$folder/config.json` (D40).
///
/// Synchronous on purpose: it runs before the async runtime starts, so a bad configuration
/// cannot get as far as binding a listener.
pub fn load(folder: &Path) -> Result<Config, ConfigError> {
    let path: PathBuf = folder.join(CONFIG_FILE_NAME);

    let buffer = std::fs::read(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;

    let config =
        serde_json::from_slice::<Config>(&buffer).map_err(|source| ConfigError::Parse {
            path: path.clone(),
            source,
        })?;

    config.validate()?;

    Ok(config)
}

#[cfg(test)]
mod tests;
