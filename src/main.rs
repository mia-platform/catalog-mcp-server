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
use crate::{cli::Cli, configuration::Configuration};

mod cli;
mod configuration;
mod logger;
mod server;
mod signal;
mod spec;
mod tracing;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    logger::try_init(env!("CARGO_BIN_NAME"))?;
    tracing::try_init()?;

    let cli = Cli::parse_args();
    let configuration = Configuration::from(&cli);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .inspect_err(|err| tracing::error!(?err, "cannot build async runtime"))?;

    rt.block_on(async {
        signal::register_shutdown_listeners();

        Ok::<_, anyhow::Error>(server::try_init(configuration).await?)
    })?;

    Ok(())
}

fn main() {
    if let Err(err) = run() {
        tracing::error!(?err, "application error");
        std::process::exit(1);
    }
}
