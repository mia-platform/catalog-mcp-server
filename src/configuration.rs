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
