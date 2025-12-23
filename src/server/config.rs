use configuration::Configuration;
use dirs::config_dir;
use once_cell::sync::Lazy;
use std::{env, path::PathBuf};

const CONFIGURATION_FOLDER_ENV_VAR: &str = "CONFIGURATION_FOLDER";

static DEFAULT_CONFIG_FOLDER: Lazy<PathBuf> =
    Lazy::new(|| config_dir().unwrap().join(env!("CARGO_BIN_NAME")));

fn get_configuration_folder() -> anyhow::Result<PathBuf> {
    match env::var(CONFIGURATION_FOLDER_ENV_VAR) {
        Ok(s) => Ok(s.into()),
        Err(env::VarError::NotPresent) => Ok(DEFAULT_CONFIG_FOLDER.clone()),
        Err(env::VarError::NotUnicode(_)) => Err(anyhow::anyhow!(
            "invalid value for {CONFIGURATION_FOLDER_ENV_VAR} environment variable"
        )),
    }
}

pub fn load_configuration() -> anyhow::Result<Configuration> {
    let configuration_folder = get_configuration_folder()?;

    let filepath = configuration_folder.join("config.json");

    let buffer = std::fs::read(&filepath)?;

    let configuration = serde_json::from_slice::<Configuration>(&buffer)?;
    tracing::debug!(?configuration, "Service configuration loaded");

    Ok(configuration)
}
