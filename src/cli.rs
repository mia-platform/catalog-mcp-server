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
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
