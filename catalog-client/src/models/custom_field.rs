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
// A `CustomField`'s spec: the key items carry its value under, the schema the value must satisfy,
// and the item types it applies to.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The group `CustomField` items live in.
pub const CUSTOM_FIELD_GROUP: &str = "mia-platform.eu";

/// The version `CustomField` items are served at.
pub const CUSTOM_FIELD_VERSION: &str = "v1";

/// The family `CustomField` items live in. The global `/items` listing excludes it.
pub const CUSTOM_FIELD_FAMILY: &str = "custom-fields";

/// What a `CustomField` defines.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct CustomFieldSpec {
    /// The key an item's `customFields` map carries the value under. Unique per tenant.
    #[serde(rename = "key")]
    pub key: String,

    /// The JSON Schema every value must satisfy.
    #[serde(rename = "schema", default)]
    pub schema: Value,

    /// The item type URNs it applies to: **absent** means every type, **empty** means none — two
    /// different answers, which is why this is not a plain `Vec`.
    #[serde(
        rename = "applicableTo",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub applicable_to: Option<Vec<String>>,
}

impl CustomFieldSpec {
    /// Whether the field applies to the item type `type_urn`, compared exactly as the engine
    /// compares it.
    pub fn applies_to(&self, type_urn: &str) -> bool {
        match &self.applicable_to {
            None => true,
            Some(types) => types.iter().any(|candidate| candidate == type_urn),
        }
    }
}
