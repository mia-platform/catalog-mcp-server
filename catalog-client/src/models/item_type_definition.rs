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
use crate::models::item::ObjectMetadata;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The names an item type is addressed and displayed by.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct TypeNames {
    /// The `kind` items of this type carry, CamelCase and singular.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The URL path segment for this type's items — the `family`.
    #[serde(rename = "plural")]
    pub plural: String,

    /// The singular form, for display.
    #[serde(rename = "singular", default, skip_serializing_if = "Option::is_none")]
    pub singular: Option<String>,

    /// The display name of one item.
    #[serde(
        rename = "displaySingular",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_singular: Option<String>,

    /// The display name of the family.
    #[serde(
        rename = "displayPlural",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_plural: Option<String>,
}

/// One served — or no longer served — version of an item type.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct TypeVersion {
    /// `v1`, `v2beta1`, and so on.
    #[serde(rename = "name")]
    pub name: String,

    /// Whether items are served under this version. **A type with no served version is not
    /// addressable** and is reported as such (§8.6).
    #[serde(rename = "served")]
    pub served: bool,

    /// Whether this version is deprecated; served-version selection prefers one that is not.
    #[serde(rename = "deprecated", default)]
    pub deprecated: Option<bool>,

    /// The JSON Schema of this version's `spec`. The largest part of an Item Type Definition by
    /// far, and the whole of what the coordinates-projection ask would remove (§17.4).
    #[serde(rename = "schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,

    /// The field selectors this version exposes, which is what T2 validates a `fields` filter
    /// against.
    #[serde(
        rename = "selectableFields",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub selectable_fields: Vec<Value>,
}

/// What an Item Type Definition declares.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemTypeDefinitionSpec {
    /// The API group items of this type are served under.
    #[serde(rename = "group")]
    pub group: String,

    /// The names this type is addressed and displayed by.
    #[serde(rename = "names")]
    pub names: TypeNames,

    /// Always `Tenant` today.
    #[serde(rename = "scope")]
    pub scope: String,

    /// The type's versions.
    #[serde(rename = "versions", default)]
    pub versions: Vec<TypeVersion>,

    /// The briefing written for a model. Returned **verbatim** when present and omitted when
    /// absent; never summarised (T1, T6).
    #[serde(
        rename = "llmDescription",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub llm_description: Option<String>,

    /// Revision-history settings, which decide whether T3 and T1 can offer history at all.
    #[serde(rename = "history", default, skip_serializing_if = "Option::is_none")]
    pub history: Option<Value>,

    /// Audit settings for reads of this type's items.
    #[serde(rename = "audit", default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<Value>,
}

/// A type registered on the catalog.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemTypeDefinition {
    /// Always `mia-platform.eu/v1`.
    #[serde(rename = "apiVersion")]
    pub api_version: String,

    /// Always `ItemTypeDefinition`.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The standard metadata. `metadata.name` is `<spec.names.plural>.<spec.group>`.
    #[serde(rename = "metadata")]
    pub metadata: ObjectMetadata,

    /// What the type declares.
    #[serde(rename = "spec")]
    pub spec: ItemTypeDefinitionSpec,

    /// The opaque optimistic-concurrency token.
    #[serde(
        rename = "resourceVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_version: Option<String>,
}

impl ItemTypeDefinitionSpec {
    /// Whether revision history is recorded for this type's items.
    pub fn history_enabled(&self) -> bool {
        self.history
            .as_ref()
            .and_then(|history| history.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// One entry of the type listing, read **only** as far as T1 needs it (T1 §5).
///
/// There is no `metadata` and nothing past the names and versions: T1 addresses a type by its
/// `spec`, never by decomposing `metadata.name`, and every field not declared here is skipped by
/// the deserialiser without being built.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItdListEntry {
    /// What the type declares.
    #[serde(rename = "spec")]
    pub spec: ItdSpec,
}

/// The part of an Item Type Definition's `spec` the type listing reads.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItdSpec {
    /// The API group items of this type are served under.
    #[serde(rename = "group")]
    pub group: String,

    /// The names this type is addressed and displayed by.
    #[serde(rename = "names")]
    pub names: TypeNames,

    /// The type's versions, without their schemas.
    #[serde(rename = "versions", default)]
    pub versions: Vec<ItdVersion>,

    /// The briefing written for a model, returned verbatim (T1 §7).
    #[serde(rename = "llmDescription", default)]
    pub llm_description: Option<String>,

    /// Revision-history settings.
    #[serde(rename = "history", default)]
    pub history: Option<ItdHistory>,
}

/// One version of a type, **without its schema** (T1-D3).
///
/// `schema` is deliberately not declared. It is ~90 % of the listing's bytes and the type
/// listing never reads it, so it is left to the deserialiser to skip — which walks it without
/// allocating a `Value` tree — rather than typed as `Option<Value>` the way the full
/// [`TypeVersion`] must, for the tools that do read it.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItdVersion {
    /// `v1`, `v2beta1`, and so on.
    #[serde(rename = "name")]
    pub name: String,

    /// Whether items are served under this version.
    #[serde(rename = "served")]
    pub served: bool,

    /// Whether this version is deprecated.
    #[serde(rename = "deprecated", default)]
    pub deprecated: Option<bool>,
}

/// A type's revision-history settings.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItdHistory {
    /// Whether revisions of this type's items are recorded. Absent means not.
    #[serde(rename = "enabled", default)]
    pub enabled: bool,
}
