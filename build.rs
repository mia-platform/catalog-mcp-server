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
// Generates `schemas/config.schema.json` from the `configuration` crate (D40), so the chart's
// own `values.schema.json` can point at a schema nobody maintains by hand.

/// Writes the JSON Schema of [`configuration::Config`] to `schemas/config.schema.json`.
fn build_configuration_schema() -> std::io::Result<()> {
    use configuration::Config;
    use schemars::{SchemaGenerator, generate::SchemaSettings};
    use std::{fs, path::Path};

    println!("cargo:rerun-if-changed=./configuration/src");

    let path = Path::new("schemas");
    fs::create_dir_all(path)?;

    let mut generator = SchemaGenerator::new(SchemaSettings::draft07());
    let schema = generator.root_schema_for::<Config>();

    fs::write(
        path.join("config.schema.json"),
        format!("{}\n", serde_json::to_string_pretty(&schema)?),
    )?;

    Ok(())
}

/// The optional build-time suffix a nightly image is stamped with, e.g. `nightly.1a2b3c4`.
const VERSION_SUFFIX_ENV: &str = "VERSION_SUFFIX";

/// The variable the binary reads its reported version from (`crate::VERSION`).
const VERSION_ENV: &str = "CATALOG_MCP_SERVER_VERSION";

/// Exposes the version the binary reports: the package version, plus `-<suffix>` when the build
/// sets [`VERSION_SUFFIX_ENV`] — so a nightly says `0.2.3-nightly.1a2b3c4` rather than claiming to
/// be the `0.2.3` release.
///
/// The suffix must be a SemVer pre-release: dot-separated identifiers of ASCII alphanumerics and
/// hyphens. Anything else fails the build rather than producing a version nobody can parse.
fn expose_version() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed={VERSION_SUFFIX_ENV}");

    let package = std::env::var("CARGO_PKG_VERSION")?;
    let version = match std::env::var(VERSION_SUFFIX_ENV) {
        Ok(suffix) if !suffix.is_empty() => {
            let valid = suffix.split('.').all(|identifier| {
                !identifier.is_empty()
                    && identifier
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
            });
            if !valid {
                return Err(
                    format!("`{VERSION_SUFFIX_ENV}={suffix}` is not a SemVer pre-release").into(),
                );
            }

            format!("{package}-{suffix}")
        }
        _ => package,
    };

    println!("cargo:rustc-env={VERSION_ENV}={version}");

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_configuration_schema()?;
    expose_version()?;

    Ok(())
}
