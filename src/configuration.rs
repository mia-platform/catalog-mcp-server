use std::fmt::Display;

use crate::{cli::Cli, spec::SpecLocation};
use rmcp_openapi::Server;
use url::Url;

#[derive(Debug, Clone)]
pub struct Configuration {
    pub spec_location: SpecLocation,
    pub base_url: Url,
    pub api_prefix: String,
    pub port: u16,
    pub ip: String,
}

impl From<&Cli> for Configuration {
    fn from(cli: &Cli) -> Self {
        Configuration {
            spec_location: cli
                .spec
                .clone()
                .unwrap_or_else(|| SpecLocation::default_from_cli_args(cli)),
            base_url: cli.base_url.clone(),
            api_prefix: cli.api_prefix.clone(),
            port: cli.port,
            ip: cli.ip.clone(),
        }
    }
}

impl Display for Configuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Configuration {{ spec_location: {}, base_url: {}, api_prefix: {}, port: {}, ip: {} }}",
            self.spec_location, self.base_url, self.api_prefix, self.port, self.ip
        )
    }
}

impl Configuration {
    pub async fn try_into_server(self) -> anyhow::Result<Server> {
        todo!()
    }
}
