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
use crate::cli::Cli;
use oas3::Spec as Oas3Spec;
use serde::de::IntoDeserializer;
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
        let mut spec_value: serde_json::Value = match self {
            SpecLocation::File(path) => {
                let content = tokio::fs::read_to_string(path).await?;
                let extension = std::path::Path::new(path)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("");

                match extension {
                    "json" => serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("{}", e)),
                    "yaml" | "yml" => {
                        let value: serde_yaml::Value =
                            serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!("{}", e))?;
                        serde_json::to_value(value).map_err(|e| anyhow::anyhow!("{}", e))
                    }
                    other => Err(anyhow::anyhow!(
                        "unsupported file extension '{}': only .json, .yaml, and .yml are supported",
                        other
                    )),
                }
            }
            SpecLocation::Url(url) => {
                let response = reqwest::get(url.clone()).await?;
                response.json().await.map_err(|e| anyhow::anyhow!("{}", e))
            }
        }?;

        let _: Oas3Spec = serde_path_to_error::deserialize(spec_value.clone().into_deserializer())
            .map_err(|err| {
                anyhow::anyhow!(
                    "invalid OpenAPI spec. JSON error at {}: {}",
                    err.path(),
                    err
                )
            })?;

        const HTTP_METHODS: &[&str] = &[
            "get", "put", "post", "delete", "options", "head", "patch", "trace",
        ];

        if let Some(paths) = spec_value.get_mut("paths").and_then(|v| v.as_object_mut()) {
            for path_item in paths.values_mut() {
                if let Some(path_obj) = path_item.as_object_mut() {
                    for method in HTTP_METHODS {
                        if let Some(operation) =
                            path_obj.get_mut(*method).and_then(|v| v.as_object_mut())
                        {
                            // Remove responses all together
                            // operation.remove("responses");

                            // keep only 2xx responses; simplify their content.*.schema
                            if let Some(responses) = operation
                                .get_mut("responses")
                                .and_then(|v| v.as_object_mut())
                            {
                                responses.retain(|status, _| status.starts_with('2'));
                                for response in responses.values_mut() {
                                    if let Some(media_types) =
                                        response.get_mut("content").and_then(|v| v.as_object_mut())
                                    {
                                        for media_type in media_types.values_mut() {
                                            if let Some(obj) = media_type.as_object_mut()
                                                && obj.contains_key("schema")
                                            {
                                                obj.insert(
                                                    "schema".to_string(),
                                                    serde_json::json!({"type": "object"}),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(spec_value)
    }
}
