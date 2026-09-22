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
use serde::Deserialize;

/// How a tool result is rendered (§11).
///
/// `structured_content` stays **off** (D15): returning every result twice — as text and as
/// structured content — doubles the metric this project exists to reduce. It is a switch, not
/// a design choice, so a client that needs it can have it without a release.
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ResponseConfig {
    /// Whether results also carry `structuredContent`.
    #[serde(default, rename = "structuredContent")]
    pub structured_content: bool,
}
