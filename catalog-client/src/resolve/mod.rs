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
    models::{ItdListEntry, ItdVersion, ItemTypeDefinition, TypeVersion},
    ops::ListQuery,
    warning::EngineWarning,
};
use serde_json::json;

/// The selector the point lookup filters on.
const KIND_SELECTOR: &str = "spec.names.kind";

/// How many near matches an unknown `kind` is answered with (T2-D9, and T3 by reference).
pub const MAX_KIND_CANDIDATES: usize = 5;

/// How many rows the `kind` lookup asks for (T6-D2): **two**. `kind` is unique per tenant, so a
/// second row cannot happen — which is exactly why asking for it is worth nothing extra. With one,
/// a broken invariant would resolve silently to whichever row came first.
const KIND_LOOKUP_LIMIT: u32 = 2;

/// Whether `kind` matches the engine's `kind` grammar, `^[a-zA-Z][a-zA-Z0-9]*$`
/// (`SPEC_KIND_PATTERN`) — checked before a lookup, so a malformed `kind` is an `invalid_input`
/// the model can act on rather than a lookup that can only find nothing.
pub fn is_valid_kind(kind: &str) -> bool {
    let mut characters = kind.chars();

    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|rest| rest.is_ascii_alphanumeric())
}

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

/// The Item Type Definition whose `spec.names.kind` is `kind`, in **one** request (P9, D30, T6-D1).
///
/// A tenant-scoped point lookup on a denormalised, filterable column — never a cache read, so it
/// cannot be stale and there is no tenant key to get wrong. The **only** place that knows this URL:
/// coordinate resolution and T6's schema read are the same request.
///
/// # Errors
///
/// - no such kind in this tenant → `not_found`, with the lookup echoed in `details`;
/// - two rows → `server_defect`, logged with both names: `kind` is unique per tenant, so the
///   catalog's own invariant is broken, and neither row may be picked (T6-D2).
pub async fn find_item_type(
    engine: &EngineClient,
    kind: &str,
) -> Result<(ItemTypeDefinition, Vec<EngineWarning>), ToolError> {
    let response = engine
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
            limit: Some(KIND_LOOKUP_LIMIT),
            field: vec![format!("{KIND_SELECTOR}={kind}")],
            ..ListQuery::default()
        })
        .await?;

    let mut definitions = response.value.items;

    match definitions.len() {
        0 => Err(ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            format!("No item type with kind `{kind}` exists in this tenant."),
        )
        .with_details(json!({ "kind": kind }))
        .with_next_step("call list_catalog_types to see the kinds that do exist")),
        1 => Ok((definitions.remove(0), response.warnings)),
        _ => {
            let names: Vec<&str> = definitions
                .iter()
                .map(|definition| definition.metadata.name.as_str())
                .collect();
            tracing::error!(
                kind,
                ?names,
                "more than one item type claims the same kind in one tenant"
            );

            Err(ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!(
                    "More than one item type claims the kind `{kind}`, which the catalog should \
                     not allow. Neither is guessed at."
                ),
            )
            .with_details(json!({ "kind": kind, "itemTypes": names })))
        }
    }
}

/// Resolves `kind → {group, version, family}` in **one** request (P9, D30).
///
/// # Errors
///
/// Those of [`find_item_type`], and `unaddressable_type` when the kind resolves but no version is
/// `served: true` — naming the versions that do exist and saying that none is served.
pub async fn resolve_kind(
    engine: &EngineClient,
    kind: &str,
) -> Result<(TypeCoordinates, Vec<EngineWarning>), ToolError> {
    let (definition, warnings) = find_item_type(engine, kind).await?;

    Ok((coordinates_of(&definition, kind)?, warnings))
}

/// [`resolve_kind`], answering an unknown `kind` with the types it was probably meant to be.
///
/// A bare `not_found` tells the model nothing, so on that path — **and only that one** — the types
/// are listed once and up to [`MAX_KIND_CANDIDATES`] near matches go into `details.candidates`
/// (T2-D9): a case-insensitive substring, in both directions, over each type's `kind`, family and
/// display name, with no edit distance. If that listing fails too, the original `not_found` is
/// returned unchanged rather than masked. The happy path costs nothing extra.
pub async fn resolve_kind_or_suggest(
    engine: &EngineClient,
    kind: &str,
) -> Result<TypeCoordinates, ToolError> {
    let definition = find_item_type_or_suggest(engine, kind).await?;

    coordinates_of(&definition, kind)
}

/// [`find_item_type`], answering an unknown `kind` with near matches — the whole definition, for
/// a tool that needs more of it than the coordinates (T6).
pub async fn find_item_type_or_suggest(
    engine: &EngineClient,
    kind: &str,
) -> Result<ItemTypeDefinition, ToolError> {
    match find_item_type(engine, kind).await {
        Ok((definition, _warnings)) => Ok(definition),
        Err(error) if error.code == codes::NOT_FOUND => {
            Err(match kind_candidates(engine, kind).await {
                Ok(candidates) if !candidates.is_empty() => {
                    error.with_details(json!({ "kind": kind, "candidates": candidates }))
                }
                _ => error,
            })
        }
        Err(error) => Err(error),
    }
}

/// The near matches for an unknown `kind`, sorted and capped.
async fn kind_candidates(engine: &EngineClient, kind: &str) -> Result<Vec<String>, ToolError> {
    let needle = kind.to_lowercase();
    let types = engine
        .list_all_item_type_definitions::<ItdListEntry>()
        .await?;

    let mut candidates: Vec<String> = types
        .into_iter()
        .filter(|entry| {
            let names = &entry.spec.names;
            [
                Some(names.kind.as_str()),
                Some(names.plural.as_str()),
                names.display_plural.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(str::to_lowercase)
            .any(|name| !name.is_empty() && (name.contains(&needle) || needle.contains(&name)))
        })
        .map(|entry| entry.spec.names.kind)
        .collect();

    candidates.sort();
    candidates.dedup();
    candidates.truncate(MAX_KIND_CANDIDATES);

    Ok(candidates)
}

/// Builds the coordinates from a resolved definition, applying the served-version rule.
///
/// Public so a tool holding the whole definition (T6) gets the same selection and the same
/// `unaddressable_type` error without a second request.
pub fn coordinates_of(
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
