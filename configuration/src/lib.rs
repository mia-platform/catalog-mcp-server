use crate::server::ServerSettings;
use schemars::JsonSchema;
use serde::Deserialize;

pub mod server;

/// The service configuration.
#[derive(Clone, Deserialize, Debug, JsonSchema)]
pub struct Configuration {
    /// The configuration for the server.
    #[serde(rename = "server", default)]
    pub server: ServerSettings,

    /// The URL of the catalog-engine.
    #[serde(rename = "catalogEngineUrl")]
    pub catalog_engine_url: String,
}
