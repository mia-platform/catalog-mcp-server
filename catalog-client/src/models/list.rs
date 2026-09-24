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

/// The `metadata` of a list envelope.
///
/// `continue` is **omitted** on the last page, which is how end-of-results is signalled.
#[derive(Clone, Default, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ListMetadata {
    /// The engine's opaque continuation token, absent on the last page.
    #[serde(rename = "continue", default)]
    pub continue_token: Option<String>,
}

/// The engine's list envelope: `{apiVersion, kind: "List", metadata: {continue?}, items}`.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ListEnvelope<T> {
    /// Always `mia-platform.eu/v1`.
    #[serde(rename = "apiVersion")]
    pub api_version: String,

    /// Always `List`.
    #[serde(rename = "kind")]
    pub kind: String,

    /// Where the next page starts, when there is one.
    #[serde(rename = "metadata", default)]
    pub metadata: ListMetadata,

    /// This page.
    #[serde(rename = "items")]
    pub items: Vec<T>,
}

/// The engine's count envelope: `{"count": <u64>}`.
#[derive(Clone, Copy, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq, Eq))]
pub struct Count {
    /// How many objects match.
    #[serde(rename = "count")]
    pub count: u64,
}
