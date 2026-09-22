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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_configuration_schema()?;

    Ok(())
}
