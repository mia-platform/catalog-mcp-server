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
use crate::{
    address::FamilyAddress,
    client::EngineClient,
    error::{Remedy, ToolError, codes},
    models::{ItdVersion, ItemTypeDefinition, TypeVersion},
    ops::ListQuery,
    warning::EngineWarning,
};
use serde_json::json;

/// The selector the point lookup filters on.
const KIND_SELECTOR: &str = "spec.names.kind";

/// Where a kind's items live, plus the two fields that come back in the same response (§8.6).
///
/// Returning `selectable_fields` and `history_enabled` here is not scope creep: they arrive in
/// the response the lookup already made, and fetching them separately would be a second request
/// for data we were handed.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeCoordinates {
    /// The API group.
    pub group: String,

    /// The served version selected by the rule below.
    pub version: String,

    /// The family — the type's `spec.names.plural`.
    pub family: String,

    /// The `kind`, as the caller asked for it.
    pub kind: String,

    /// The Item Type Definition's own `metadata.name`, for addressing the type itself.
    pub type_name: String,

    /// The field selectors the chosen version exposes, which is what a `fields` filter is
    /// validated against.
    pub selectable_fields: Vec<String>,

    /// Whether revision history is recorded for this type's items.
    pub history_enabled: bool,
}

impl TypeCoordinates {
    /// The validated address of this type's items, for listing or counting them.
    pub fn family_address(&self) -> Result<FamilyAddress, ToolError> {
        FamilyAddress::new(&self.group, &self.version, &self.family)
    }
}

/// Resolves `kind → {group, version, family}` in **one** request (P9, D30).
///
/// A tenant-scoped point lookup, never a cache read: there is no cache in v1, so this cannot be
/// stale and there is no tenant key to get wrong. `404`-loud rather than silently wrong.
///
/// # Errors
///
/// - no such kind in this tenant → `not_found`, with the lookup echoed in `details`;
/// - the kind resolves but no version is `served: true` → `unaddressable_type`, naming the
///   versions that do exist and saying that none is served.
pub async fn resolve_kind(
    engine: &EngineClient,
    kind: &str,
) -> Result<(TypeCoordinates, Vec<EngineWarning>), ToolError> {
    let response = engine
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
            limit: Some(1),
            field: vec![format!("{KIND_SELECTOR}={kind}")],
            ..ListQuery::default()
        })
        .await?;

    let definition = response.value.items.into_iter().next().ok_or_else(|| {
        ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            format!("No item type with kind `{kind}` exists in this tenant."),
        )
        .with_details(json!({ "kind": kind }))
        .with_next_step("call list_catalog_types to see the kinds that do exist")
    })?;

    Ok((coordinates_of(&definition, kind)?, response.warnings))
}

/// Builds the coordinates from a resolved definition, applying the served-version rule.
fn coordinates_of(
    definition: &ItemTypeDefinition,
    kind: &str,
) -> Result<TypeCoordinates, ToolError> {
    let version = select_served_version(&definition.spec.versions).ok_or_else(|| {
        let existing: Vec<&str> = definition
            .spec
            .versions
            .iter()
            .map(|version| version.name.as_str())
            .collect();

        ToolError::new(
            codes::UNADDRESSABLE_TYPE,
            Remedy::Escalate,
            format!(
                "The item type `{kind}` exists but none of its versions is served, so its items \
                 cannot be addressed."
            ),
        )
        .with_details(json!({ "kind": kind, "versions": existing, "servedVersions": [] }))
    })?;

    Ok(TypeCoordinates {
        group: definition.spec.group.clone(),
        version: version.name.clone(),
        family: definition.spec.names.plural.clone(),
        kind: kind.to_string(),
        type_name: definition.metadata.name.clone(),
        selectable_fields: version
            .selectable_fields
            .iter()
            .filter_map(|field| field.get("jsonPath").and_then(|path| path.as_str()))
            .map(str::to_string)
            .collect(),
        history_enabled: definition.spec.history_enabled(),
    })
}

/// What version selection reads from a version, whichever model it was deserialised into.
///
/// The rule of [`select_served_version`] **lives in one place** (§8.6, T1-D4), but two models
/// carry versions: the full [`TypeVersion`], schema and all, and T1's lean [`ItdVersion`], which
/// skips the schema (T1-D3). This is what lets both reach the same rule rather than a copy.
pub trait ServedVersion {
    /// `v1`, `v2beta1`, and so on.
    fn name(&self) -> &str;

    /// Whether items are served under this version.
    fn served(&self) -> bool;

    /// Whether this version is deprecated. Absent means not.
    fn deprecated(&self) -> bool;
}

impl ServedVersion for TypeVersion {
    fn name(&self) -> &str {
        &self.name
    }

    fn served(&self) -> bool {
        self.served
    }

    fn deprecated(&self) -> bool {
        self.deprecated.unwrap_or(false)
    }
}

impl ServedVersion for ItdVersion {
    fn name(&self) -> &str {
        &self.name
    }

    fn served(&self) -> bool {
        self.served
    }

    fn deprecated(&self) -> bool {
        self.deprecated.unwrap_or(false)
    }
}

/// Picks the version items are addressed under (§8.6, from T1).
///
/// The rule, in order: consider only `served: true`; prefer one that is not `deprecated`; then
/// the highest stability and number — `v2` > `v1` > `v2beta1` > `v1alpha1`.
///
/// **Today every shipped type has exactly one served `v1`, so this never fires.** It is cheap,
/// and it is the kind of rule that fires in production first.
pub fn select_served_version<V: ServedVersion>(versions: &[V]) -> Option<&V> {
    versions
        .iter()
        .filter(|version| version.served())
        .max_by_key(|version| {
            (
                // A version that is not deprecated always wins.
                u8::from(!version.deprecated()),
                stability_rank(version.name()),
                major_of(version.name()),
                minor_of(version.name()),
            )
        })
}

/// How stable a version name claims to be: stable beats beta beats alpha.
fn stability_rank(name: &str) -> u8 {
    if name.contains("alpha") {
        0
    } else if name.contains("beta") {
        1
    } else {
        2
    }
}

/// The major number in `v<major>[alpha|beta<minor>]`.
fn major_of(name: &str) -> u32 {
    name.trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// The pre-release number in `v<major>[alpha|beta<minor>]`, or zero for a stable version.
fn minor_of(name: &str) -> u32 {
    name.rsplit(|c: char| c.is_ascii_alphabetic())
        .next()
        .filter(|digits| {
            !digits.is_empty() && name.chars().any(|c| c.is_ascii_alphabetic() && c != 'v')
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
