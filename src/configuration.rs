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
use crate::{cli::Cli, spec::SpecLocation};
use anyhow::Context;
use rmcp_openapi::{
    Server,
    spec::{Filter, Filters},
};
use std::{fmt::Display, net::IpAddr};
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
    pub allowed_hosts: Option<Vec<String>>,
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
            allowed_hosts: cli.allowed_hosts.clone(),
        }
    }
}

impl Display for Configuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Configuration {{ spec_location: {}, base_url: {}, transport_mode: {:?}, api_prefix: {}, port: {}, ip: {}, allowed_hosts: {:?} }}",
            self.spec_location,
            self.base_url,
            self.transport_mode,
            self.api_prefix,
            self.port,
            self.ip,
            self.allowed_hosts
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

        let server = Server::builder()
            .name(env!("CARGO_PKG_NAME").to_string())
            .version(env!("CARGO_PKG_VERSION").to_string())
            .instructions(env!("CARGO_PKG_DESCRIPTION").to_string())
            .openapi_spec(openapi_spec)
            .base_url(self.base_url.clone())
            .filters(
                Filters::builder()
                    .tags(Filter::Exclude(vec!["organizations".to_string()]))
                    .build(),
            )
            .build();

        Ok(server)
    }
}
