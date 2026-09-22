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
use std::net::{IpAddr, Ipv4Addr};

/// Default bind address: every interface, as the container has no other route in.
pub static DEFAULT_IP_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));

/// Default listening port, matching the chart's `containerPort`.
pub static DEFAULT_HTTP_PORT: u16 = 8000;

/// Default path the MCP service is mounted at — what the gateway already routes.
pub static DEFAULT_MCP_PATH: &str = "/mcp";

/// Default maximum accepted POST body, a deliberate tightening of the SDK's 4 MiB
/// (§6.4): a tool argument set is kilobytes, and anything larger is abuse.
pub static DEFAULT_MAX_BODY_BYTES: usize = 1_048_576;

/// Returns [`DEFAULT_IP_ADDR`].
pub fn default_ip_addr() -> IpAddr {
    DEFAULT_IP_ADDR
}

/// Returns [`DEFAULT_HTTP_PORT`].
pub fn default_http_port() -> u16 {
    DEFAULT_HTTP_PORT
}

/// Returns [`DEFAULT_MCP_PATH`].
pub fn default_mcp_path() -> String {
    DEFAULT_MCP_PATH.to_string()
}

/// Returns [`DEFAULT_MAX_BODY_BYTES`].
pub fn default_max_body_bytes() -> usize {
    DEFAULT_MAX_BODY_BYTES
}

#[cfg(feature = "json-schema")]
fn ip_addr_json_schema(_: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
    use ::schemars::json_schema;

    json_schema!({
        "type": "string",
        "format": "ipv4",
    })
}

/// How the HTTP listener is bound and what the transport accepts (§11).
///
/// `allowed_hosts` has **no usable default**: the SDK's own default is loopback-only, so a
/// remote deployment answers every request `403 Forbidden: Host header is not allowed` until
/// the list is set. Validation therefore refuses an empty list before the listener binds (D11).
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ServerConfig {
    /// Server bind IP.
    #[serde(default = "default_ip_addr", rename = "ip")]
    #[cfg_attr(feature = "json-schema", schemars(schema_with = "ip_addr_json_schema"))]
    pub ip: IpAddr,

    /// Server bind port.
    #[serde(default = "default_http_port", rename = "port")]
    pub port: u16,

    /// Path the MCP service is mounted at.
    #[serde(default = "default_mcp_path", rename = "mcpPath")]
    pub mcp_path: String,

    /// Hostnames or `host:port` authorities accepted in the inbound `Host` header.
    /// Required and non-empty — see the type documentation.
    #[serde(default, rename = "allowedHosts")]
    pub allowed_hosts: Vec<String>,

    /// Browser origins accepted in the inbound `Origin` header. Empty leaves `Origin`
    /// validation switched off, which is what the in-cluster client needs (D11).
    #[serde(default, rename = "allowedOrigins")]
    pub allowed_origins: Vec<String>,

    /// Maximum accepted POST body size, in bytes.
    #[serde(default = "default_max_body_bytes", rename = "maxBodyBytes")]
    pub max_body_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            ip: default_ip_addr(),
            port: default_http_port(),
            mcp_path: default_mcp_path(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            max_body_bytes: default_max_body_bytes(),
        }
    }
}
