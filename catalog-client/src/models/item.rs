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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A link on an object's metadata.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct Link {
    /// The link's display text.
    #[serde(rename = "title")]
    pub title: String,

    /// Where it points.
    #[serde(rename = "url")]
    pub url: String,
}

/// The standard object metadata every catalog object carries.
///
/// `BTreeMap` for the map-valued fields so serialisation is deterministic and a byte golden
/// means something (§3.2).
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ObjectMetadata {
    /// The object name, unique within its family and tenant.
    #[serde(rename = "name")]
    pub name: String,

    /// The family this object belongs to, which is its Item Type Definition's
    /// `spec.names.plural`. **`None` only for an object whose type no longer exists** — which is
    /// an unaddressable item, not a missing one (D30).
    #[serde(rename = "family", default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,

    /// A human-readable title.
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// A brief description.
    #[serde(
        rename = "description",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<String>,

    /// Queryable key/value labels.
    #[serde(rename = "labels", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,

    /// Non-queryable key/value annotations, preserved across modifications.
    #[serde(
        rename = "annotations",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub annotations: BTreeMap<String, String>,

    /// Free-form tags.
    #[serde(rename = "tags", default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Related links.
    #[serde(rename = "links", default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,

    /// The stable catalog URN.
    #[serde(rename = "urn", default, skip_serializing_if = "Option::is_none")]
    pub urn: Option<String>,

    /// The server-assigned unique id.
    #[serde(rename = "uid", default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,

    /// When the object was created, in ISO 8601 UTC.
    #[serde(
        rename = "creationTimestamp",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub creation_timestamp: Option<String>,

    /// When the object was last updated, in ISO 8601 UTC.
    #[serde(
        rename = "updateTimestamp",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub update_timestamp: Option<String>,

    /// The object's owner, as an identity reference. Not writable by an agent (Q6).
    #[serde(rename = "owner", default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Value>,

    /// The object's followers. Not writable by an agent (Q6).
    #[serde(rename = "followers", default, skip_serializing_if = "Vec::is_empty")]
    pub followers: Vec<Value>,
}

/// An entity recorded on the catalog.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct Item {
    /// `<group>/<version>`.
    #[serde(rename = "apiVersion")]
    pub api_version: String,

    /// The object's type.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The standard metadata.
    #[serde(rename = "metadata")]
    pub metadata: ObjectMetadata,

    /// The type-specific state.
    #[serde(rename = "spec")]
    pub spec: Value,

    /// Values keyed by a `CustomField` entity's `spec.key`. Written through its own endpoint,
    /// never through `PUT` (T10).
    #[serde(
        rename = "customFields",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_fields: Option<Value>,

    /// The opaque optimistic-concurrency token. **Top-level**, not inside `metadata`.
    #[serde(
        rename = "resourceVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_version: Option<String>,
}

/// The metadata-only projection: no `spec`, but the **full** `ObjectMetadata`.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct PartialObjectMetadata {
    /// `<group>/<version>`.
    #[serde(rename = "apiVersion")]
    pub api_version: String,

    /// The object's type.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The standard metadata, in full.
    #[serde(rename = "metadata")]
    pub metadata: ObjectMetadata,

    /// The opaque optimistic-concurrency token.
    #[serde(
        rename = "resourceVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_version: Option<String>,
}
