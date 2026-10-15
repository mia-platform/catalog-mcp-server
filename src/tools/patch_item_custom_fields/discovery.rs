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
// What the tenant's custom fields allow on one item type: the listing of every definition, the
// keys this type can take, and — when the engine refuses a patch — what each key of it is.
//
// The engine refuses an unknown key and a key meant for other types with the same message, and
// reports only the first, so the answer the model needs is worked out here, from the definitions
// themselves, and never from the wording of the engine's message.

use catalog_client::{
    EngineClient, FamilyAddress, ToolError, TypeCoordinates,
    models::{
        CustomFieldSpec,
        custom_field::{CUSTOM_FIELD_FAMILY, CUSTOM_FIELD_GROUP, CUSTOM_FIELD_VERSION},
    },
    ops::ListQuery,
    pagination::{MAX_LIMIT, paginate_all},
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The scheme every catalog URN starts with.
const URN_PREFIX: &str = "urn:mia-platform-catalog:";

/// The URN the engine matches `applicableTo` against: the group and version the patch is
/// addressed at, and the type's kind — so a field listed for another version of the same type does
/// not apply, exactly as the engine decides.
pub fn type_urn(coordinates: &TypeCoordinates) -> String {
    format!(
        "{URN_PREFIX}{}:{}:{}",
        coordinates.group, coordinates.version, coordinates.kind
    )
}

/// Every `CustomField` definition of the tenant, every page of the family listing.
///
/// The listing is unfiltered: on `applicableTo` the engine's filter cannot tell "every type" from
/// "no type", and a classification needs the fields of other types too. A definition whose spec
/// cannot be read is left out and logged, rather than failing the whole answer.
pub async fn list_definitions(engine: &EngineClient) -> Result<Vec<CustomFieldSpec>, ToolError> {
    let family = FamilyAddress::new(
        CUSTOM_FIELD_GROUP,
        CUSTOM_FIELD_VERSION,
        CUSTOM_FIELD_FAMILY,
    )?;
    let family = &family;

    let items = paginate_all(|cursor| {
        let query = ListQuery {
            limit: Some(MAX_LIMIT),
            cursor,
            ..ListQuery::default()
        };

        async move {
            engine
                .list_family_items(family, &query)
                .await
                .map(|response| response.value)
        }
    })
    .await?;

    Ok(items
        .into_iter()
        .filter_map(|item| {
            serde_json::from_value::<CustomFieldSpec>(item.spec)
                .inspect_err(|error| {
                    tracing::warn!(
                        name = item.metadata.name,
                        ?error,
                        "a custom field definition could not be read; it is left out"
                    );
                })
                .ok()
        })
        .collect())
}

/// The keys an item of the type `type_urn` can take, each with the schema its value must satisfy,
/// sorted by key.
pub fn valid_keys<'a>(
    definitions: &'a [CustomFieldSpec],
    type_urn: &str,
    only: Option<&[&str]>,
) -> Map<String, Value> {
    let sorted: BTreeMap<&'a str, &'a Value> = definitions
        .iter()
        .filter(|definition| definition.applies_to(type_urn))
        .filter(|definition| only.is_none_or(|keys| keys.contains(&definition.key.as_str())))
        .map(|definition| (definition.key.as_str(), &definition.schema))
        .collect();

    sorted
        .into_iter()
        .map(|(key, schema)| (key.to_string(), schema.clone()))
        .collect()
}

/// What one key of a refused patch is, for this item's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyClass {
    /// A field that applies to this type.
    Valid,

    /// No field has this key.
    Unknown,

    /// A field for other item types, which it lists.
    OtherTypes(Vec<String>),

    /// A field defined to apply to no item type at all.
    NoType,
}

impl KeyClass {
    /// Whether the model could still write something with this key — as it is, or once corrected.
    pub fn is_writable(&self) -> bool {
        matches!(self, Self::Valid | Self::Unknown)
    }

    /// How the key is reported in `details.keys`.
    fn to_json(&self) -> Value {
        match self {
            Self::Valid => json!("valid"),
            Self::Unknown => json!("unknown"),
            Self::OtherTypes(types) => json!({ "otherTypes": types }),
            Self::NoType => json!("noType"),
        }
    }
}

/// What `key` is for the type `type_urn`, matched by `spec.key` — never by the definition's name,
/// which the engine does not look at.
pub fn classify(definitions: &[CustomFieldSpec], key: &str, type_urn: &str) -> KeyClass {
    match definitions.iter().find(|definition| definition.key == key) {
        None => KeyClass::Unknown,
        Some(definition) if definition.applies_to(type_urn) => KeyClass::Valid,
        Some(CustomFieldSpec {
            applicable_to: Some(types),
            ..
        }) if !types.is_empty() => KeyClass::OtherTypes(types.clone()),
        Some(_) => KeyClass::NoType,
    }
}

/// `details.keys`: each classified key, sorted.
pub fn keys_json(classes: &BTreeMap<String, KeyClass>) -> Value {
    Value::Object(
        classes
            .iter()
            .map(|(key, class)| (key.clone(), class.to_json()))
            .collect(),
    )
}

/// One sentence saying why `key` was refused; `None` for a valid key.
pub fn explain(key: &str, class: &KeyClass, coordinates: &TypeCoordinates) -> Option<String> {
    match class {
        KeyClass::Valid => None,
        KeyClass::Unknown => Some(format!("`{key}` is not a custom field of this tenant.")),
        KeyClass::NoType => Some(format!(
            "`{key}` is defined but applies to no item type: its definition must be changed."
        )),
        KeyClass::OtherTypes(types) => {
            let other_versions = other_versions_of(types, coordinates);
            if other_versions.is_empty() {
                Some(format!(
                    "`{key}` is a custom field for other item types ({}), not for `{}`.",
                    kinds_of(types).join(", "),
                    coordinates.kind
                ))
            } else {
                Some(format!(
                    "`{key}` is defined for another version of `{}` ({}); this item is addressed \
                     at `{}`.",
                    coordinates.kind,
                    other_versions.join(", "),
                    coordinates.version
                ))
            }
        }
    }
}

/// The versions in `types` that name this very group and kind at a version other than the one
/// addressed.
fn other_versions_of<'a>(types: &'a [String], coordinates: &TypeCoordinates) -> Vec<&'a str> {
    types
        .iter()
        .filter_map(|urn| parse_type_urn(urn))
        .filter(|(group, version, kind)| {
            *group == coordinates.group
                && *kind == coordinates.kind
                && *version != coordinates.version
        })
        .map(|(_, version, _)| version)
        .collect()
}

/// The kind each type URN names, or the URN itself when it cannot be read.
fn kinds_of(types: &[String]) -> Vec<&str> {
    types
        .iter()
        .map(|urn| parse_type_urn(urn).map_or(urn.as_str(), |(_, _, kind)| kind))
        .collect()
}

/// `urn:mia-platform-catalog:<group>:<version>:<kind>` → its three parts.
fn parse_type_urn(urn: &str) -> Option<(&str, &str, &str)> {
    let mut parts = urn.strip_prefix(URN_PREFIX)?.split(':');

    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(group), Some(version), Some(kind), None) => Some((group, version, kind)),
        _ => None,
    }
}
