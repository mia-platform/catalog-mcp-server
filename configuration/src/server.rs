use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr};

pub static DEFAULT_API_PREFIX: &str = "/";
pub static DEFAULT_HTTP_PORT: u16 = 3000;
pub static DEFAULT_IP_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));

pub fn default_http_port() -> u16 {
    DEFAULT_HTTP_PORT
}

pub fn default_ip_addr() -> IpAddr {
    DEFAULT_IP_ADDR
}

pub fn default_api_prefix() -> String {
    DEFAULT_API_PREFIX.to_string()
}

fn ip_addr_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    use schemars::json_schema;

    json_schema!({
        "type": "string",
        "format": "ipv4",
    })
}

fn api_prefix_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    use schemars::json_schema;

    json_schema!({
        "type": "string",
        "pattern": "^/(?:[a-zA-Z0-9._~-]+(?:/[a-zA-Z0-9._~-]+)*)?$",
    })
}

#[derive(Clone, Deserialize, Debug, PartialEq, schemars::JsonSchema)]
pub struct ServerSettings {
    /// Server bind IP
    #[serde(default = "default_ip_addr", rename = "ip")]
    #[schemars(schema_with = "ip_addr_json_schema")]
    pub ip: IpAddr,

    /// Server port
    #[serde(default = "default_http_port", rename = "port")]
    pub port: u16,

    /// Server REST API prefix
    #[serde(default = "default_api_prefix", rename = "apiPrefix")]
    #[schemars(schema_with = "api_prefix_json_schema")]
    pub api_prefix: String,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            ip: default_ip_addr(),
            port: default_http_port(),
            api_prefix: default_api_prefix(),
        }
    }
}
