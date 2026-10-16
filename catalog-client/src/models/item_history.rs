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
// An item's history as the engine records it: revisions, each a record of one recorded state, and
// versions, a SemVer name given to one revision. Only the fields read are declared, and every one
// a version's answer may leave out is optional, so a tag that exists is never misread.

use serde::{Deserialize, Serialize};

/// Who recorded a revision, or cut a version.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct HistoryAuthor {
    /// The principal's id.
    #[serde(rename = "id")]
    pub id: String,

    /// The principal's name, when it has resolved.
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
}

/// One recorded state of an item — the record of it, not the item's content.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemRevision {
    /// The revision's identity and time.
    #[serde(rename = "metadata")]
    pub metadata: ItemRevisionMetadata,

    /// Why it was recorded.
    #[serde(rename = "spec", default)]
    pub spec: Option<ItemRevisionSpec>,
}

/// Why a revision was recorded — only what is read.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemRevisionSpec {
    /// The version a restore wrote this revision from; absent on any other revision.
    #[serde(rename = "restoredFrom", default)]
    pub restored_from: Option<String>,
}

/// A revision's identity and time.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemRevisionMetadata {
    /// The revision's id — what a version tags.
    #[serde(rename = "uid")]
    pub uid: String,

    /// The state's time: when it was recorded, or, for a reconstructed revision, the time it stands for.
    #[serde(rename = "creationTimestamp")]
    pub creation_timestamp: String,
}

/// A SemVer name given to one revision of an item.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemVersion {
    /// The version's name and who cut it, when.
    #[serde(rename = "metadata")]
    pub metadata: ItemVersionMetadata,

    /// The revision it tags.
    #[serde(rename = "spec", default)]
    pub spec: Option<ItemVersionSpec>,
}

/// A version's name, and who cut it when.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemVersionMetadata {
    /// The SemVer name, unique per item.
    #[serde(rename = "name")]
    pub name: String,

    /// When the version was cut.
    #[serde(rename = "creationTimestamp", default)]
    pub creation_timestamp: Option<String>,

    /// Who cut it — not who wrote the revision it tags.
    #[serde(rename = "author", default)]
    pub author: Option<HistoryAuthor>,
}

/// What a version tags.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemVersionSpec {
    /// The id of the tagged revision.
    #[serde(rename = "revisionId", default)]
    pub revision_id: Option<String>,
}
