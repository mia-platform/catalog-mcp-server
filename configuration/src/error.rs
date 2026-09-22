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
use std::path::PathBuf;

/// Field path of the `Host` allowlist (D11).
pub const FIELD_SERVER_ALLOWED_HOSTS: &str = "server.allowedHosts";

/// Field path of the identity posture (D46).
pub const FIELD_AUTH_MODE: &str = "auth.mode";

/// Field path of the canonical resource URI (§7.3).
pub const FIELD_AUTH_RESOURCE: &str = "auth.resource";

/// Field path of the gateway base URL (D27, D48).
pub const FIELD_ENGINE_BASE_URL: &str = "engine.baseUrl";

/// Field path of the per-call wall-clock budget (§11).
pub const FIELD_TOOLS_CALL_DEADLINE_SECONDS: &str = "tools.callDeadlineSeconds";

/// Why the configuration could not be loaded, at the one boundary where it is read.
///
/// A failure here exits the process non-zero **before the listener binds** (D40), and every
/// variant carries the thing an operator needs to act: the file that could not be read or
/// parsed, or the field path that is wrong.
#[derive(Debug)]
pub enum ConfigError {
    /// The configuration file could not be read.
    Read {
        /// The file that was attempted.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The configuration file is not valid JSON, or does not match the schema.
    Parse {
        /// The file that was attempted.
        path: PathBuf,
        /// The underlying deserialisation failure, which carries its own field path.
        source: serde_json::Error,
    },

    /// The configuration parsed, but a validation rule refuses it (§11).
    Invalid {
        /// The dotted field path, as it is written in `config.json`.
        field: &'static str,
        /// One sentence saying what is wrong and what to do about it.
        reason: String,
    },
}

impl ConfigError {
    /// Builds an [`ConfigError::Invalid`] for `field`.
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(
                f,
                "cannot read the configuration file at {}: {source}",
                path.display()
            ),
            Self::Parse { path, source } => write!(
                f,
                "cannot parse the configuration file at {}: {source}",
                path.display()
            ),
            Self::Invalid { field, reason } => {
                write!(f, "invalid configuration: `{field}`: {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Invalid { .. } => None,
        }
    }
}
