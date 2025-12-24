use crate::cli::Cli;
use std::{fmt::Display, path::PathBuf, str::FromStr};
use url::Url;

pub static CATALOG_SPEC_PATH: &str = "openapi/json";

#[derive(Debug, Clone)]
pub enum SpecLocation {
    File(String),
    Url(Url),
}

impl FromStr for SpecLocation {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("http://") || s.starts_with("https://") {
            Url::parse(s)
                .map(SpecLocation::Url)
                .map_err(|e| anyhow::anyhow!("{}", e))
        } else {
            let path_buf = PathBuf::from(s);
            let path_str = path_buf
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid file path encoding: {}", s))?;

            Ok(SpecLocation::File(path_str.to_string()))
        }
    }
}

impl Display for SpecLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecLocation::File(path) => write!(f, "File({})", path),
            SpecLocation::Url(url) => write!(f, "Url({})", url),
        }
    }
}

impl SpecLocation {
    pub fn default_from_cli_args(args: &Cli) -> Self {
        let mut base_url = args.base_url.clone();

        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }

        let full_url = base_url
            .join(CATALOG_SPEC_PATH)
            .expect("cannot build default spec URL from base URL");

        SpecLocation::Url(full_url)
    }

    pub async fn load_spec(&self) -> Result<serde_json::Value, anyhow::Error> {
        match self {
            SpecLocation::File(path) => {
                let content = tokio::fs::read_to_string(path).await?;
                let spec: serde_json::Value = serde_json::from_str(&content)?;
                Ok(spec)
            }
            SpecLocation::Url(url) => {
                let response = reqwest::get(url.clone()).await?;
                let spec: serde_json::Value = response.json().await?;
                Ok(spec)
            }
        }
    }
}
