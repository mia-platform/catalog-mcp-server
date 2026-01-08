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
use std::{fmt::Display, net::IpAddr};

use crate::{cli::Cli, spec::SpecLocation};
use anyhow::Context;
use rmcp_openapi::Server;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    Stdio,
    Http,
}

#[derive(Debug, Clone)]
pub struct Configuration {
    pub spec_location: SpecLocation,
    pub base_url: Url,
    pub transport_mode: TransportMode,
    pub api_prefix: String,
    pub port: u16,
    pub ip: IpAddr,
}

impl From<&Cli> for Configuration {
    fn from(cli: &Cli) -> Self {
        Configuration {
            spec_location: cli
                .spec
                .clone()
                .unwrap_or_else(|| SpecLocation::default_from_cli_args(cli)),
            base_url: cli.base_url.clone(),
            transport_mode: if cli.stdio {
                TransportMode::Stdio
            } else {
                TransportMode::Http
            },
            api_prefix: cli.api_prefix.clone(),
            port: cli.port,
            ip: cli.ip,
        }
    }
}

impl Display for Configuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Configuration {{ spec_location: {}, base_url: {}, transport_mode: {:?}, api_prefix: {}, port: {}, ip: {} }}",
            self.spec_location,
            self.base_url,
            self.transport_mode,
            self.api_prefix,
            self.port,
            self.ip
        )
    }
}

impl Configuration {
    pub async fn try_into_server(&self) -> anyhow::Result<Server> {
        let openapi_spec = self
            .spec_location
            .load_spec()
            .await
            .with_context(|| "while loading OpenAPI spec")?;

        let mut server = Server::new(
            openapi_spec,
            self.base_url.clone(),
            None,
            None,
            false,
            false,
        );

        server.name = Some(env!("CARGO_PKG_NAME").to_string());
        server.version = Some(env!("CARGO_PKG_VERSION").to_string());
        server.instructions = Some(env!("CARGO_PKG_DESCRIPTION").to_string());

        Ok(server)
    }
}
