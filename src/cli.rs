/**
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
use crate::spec::{CATALOG_SPEC_PATH, SpecLocation};
use clap::Parser;
use std::net::IpAddr;
use url::Url;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
#[command(name = "catalog-mcp-server")]
pub struct Cli {
    #[arg(
        long,
        short = 's',
        value_name = "LOCATION",
        help = format!("Path or URL to the OpenAPI specification file from which the MCP server should be built [default: <base-url>/{}]", CATALOG_SPEC_PATH)
    )]
    pub spec: Option<SpecLocation>,

    /// Mia-Platform Catalog base URL
    #[arg(long, short = 'b', value_name = "URL")]
    pub base_url: Url,

    /// Use stdio transport instead of HTTP streaming
    #[arg(long, default_value = "false")]
    pub stdio: bool,

    /// Prefix for the MCP server REST API
    #[arg(long, value_name = "PREFIX", default_value = "/")]
    pub api_prefix: String,

    /// Port to bind the MCP server to
    #[arg(long, short = 'p', default_value = "8000")]
    pub port: u16,

    /// IP address to bind the MCP server to
    #[arg(long, default_value = "0.0.0.0")]
    pub ip: IpAddr,

    /// Allowed hostnames or host:port authorities for inbound Host header validation.
    /// If not set, the server defaults to loopback hosts only (localhost, 127.0.0.1, ::1).
    /// Pass with no values (--allowed-hosts) to allow any host.
    /// Pass one or more values to restrict to those hosts.
    #[arg(long, num_args(0..), value_name = "HOST")]
    pub allowed_hosts: Option<Vec<String>>,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
