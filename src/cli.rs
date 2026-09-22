/*
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
use clap::Parser;
use std::{path::PathBuf, sync::LazyLock};

/// Environment variable naming the folder that holds `config.json` (D40, engine parity).
const CONFIGURATION_FOLDER_ENV_VAR: &str = "CONFIGURATION_FOLDER";

/// Where the configuration is looked for when neither the flag nor the environment says.
static DEFAULT_CONFIG_FOLDER: LazyLock<PathBuf> = LazyLock::new(|| {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(env!("CARGO_BIN_NAME"))
});

/// The whole command-line surface (D40).
///
/// `--spec` and `--base-url` are gone with the OpenAPI generator they existed to feed: the
/// server is configured by a JSON file, so a deployment change is not a release.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
#[command(name = "catalog-mcp-server")]
pub struct Cli {
    /// Folder holding `config.json`
    #[arg(
        long,
        value_name = "FOLDER",
        env = CONFIGURATION_FOLDER_ENV_VAR,
        default_value_os_t = DEFAULT_CONFIG_FOLDER.clone(),
    )]
    pub config_folder: PathBuf,
}

impl Cli {
    /// Parses the process arguments, exiting with clap's own message on a bad invocation.
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
