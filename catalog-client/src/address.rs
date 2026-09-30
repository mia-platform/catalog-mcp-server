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
use crate::error::{Remedy, ToolError, codes};
use regex::Regex;
use std::sync::LazyLock;

/// The engine's own `metadata.name` pattern.
static NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern, copied from the engine's OAS.
    Regex::new(r"^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$").expect("NAME_RE is a constant pattern")
});

/// The engine's own `metadata.family` and `spec.names.plural` pattern.
static FAMILY_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern, copied from the engine's OAS.
    Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$").expect("FAMILY_RE is a constant pattern")
});

/// The engine's own `spec.group` pattern.
static GROUP_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern, copied from the engine's OAS.
    Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$")
        .expect("GROUP_RE is a constant pattern")
});

/// The version half of an `apiVersion`.
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern, copied from the engine's OAS.
    Regex::new(r"^v[0-9]+(alpha[0-9]+|beta[0-9]+)?$").expect("VERSION_RE is a constant pattern")
});

/// The engine's `maxLength` on a group, and the practical ceiling on a name.
const MAX_SEGMENT_LENGTH: usize = 253;

/// Where an item lives, and the only thing the write helper accepts (§8.1).
///
/// Built either from a manifest already in hand — `apiVersion` plus `metadata.family`, the
/// common case — or by `kind → {group, version, family}` resolution.
///
/// **Every segment is validated against the engine's own regexes before a request is built**, so
/// a malformed name is a tool error the model can act on rather than a `400` round trip it has
/// to interpret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemAddress {
    group: String,
    version: String,
    family: String,
    name: String,
}

impl ItemAddress {
    /// Validates and builds an address from its four parts.
    pub fn new(
        group: impl Into<String>,
        version: impl Into<String>,
        family: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Self, ToolError> {
        let (group, version, family, name) =
            (group.into(), version.into(), family.into(), name.into());

        validate("group", &group, &GROUP_RE)?;
        validate("version", &version, &VERSION_RE)?;
        validate("family", &family, &FAMILY_RE)?;
        validate("name", &name, &NAME_RE)?;

        Ok(Self {
            group,
            version,
            family,
            name,
        })
    }

    /// Builds an address from a manifest already in hand.
    ///
    /// `family` is `None` **only** for an object whose Item Type Definition no longer exists —
    /// a real engine state, not a lookup miss — so it is reported as `unaddressable_item` and
    /// never as an empty result (D30).
    pub fn from_manifest(
        api_version: &str,
        family: Option<&str>,
        name: &str,
    ) -> Result<Self, ToolError> {
        let Some(family) = family else {
            return Err(ToolError::new(
                codes::UNADDRESSABLE_ITEM,
                Remedy::Escalate,
                format!(
                    "The object `{name}` has no family, which means its item type no longer \
                     exists. It cannot be addressed until the type is restored."
                ),
            )
            .with_details(serde_json::json!({ "name": name, "apiVersion": api_version })));
        };

        let (group, version) = api_version.split_once('/').ok_or_else(|| {
            ToolError::new(
                codes::INVALID_INPUT,
                Remedy::RetryAfterChange,
                format!(
                    "`{api_version}` is not a valid apiVersion: it must be `<group>/<version>`."
                ),
            )
        })?;

        Self::new(group, version, family, name)
    }

    /// The API group.
    pub fn group(&self) -> &str {
        &self.group
    }

    /// The API version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The family, which is the item type's `spec.names.plural`.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// The object name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `apiVersion` this address came from, or resolves to.
    pub fn api_version(&self) -> String {
        format!("{}/{}", self.group, self.version)
    }

    /// The path segments, in order, for `url::Url::path_segments_mut`.
    ///
    /// Segments are handed to `url`, which percent-encodes them; they are never formatted into
    /// a string. Every one has already been validated, so there is nothing here that could
    /// traverse a path even if the encoder were removed.
    pub fn segments(&self) -> [&str; 5] {
        [
            &self.group,
            &self.version,
            "items",
            &self.family,
            &self.name,
        ]
    }
}

impl std::fmt::Display for ItemAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "/{}/{}/items/{}/{}",
            self.group, self.version, self.family, self.name
        )
    }
}

/// Where a **family** of items lives: `/{group}/{version}/items/{family}` (§8.1).
///
/// An [`ItemAddress`] without the name, for the operations that act on a whole family — listing
/// it and counting it. Validated the same way, segment by segment, so coordinates that come back
/// inside a tool's cursor are checked before they become a path, exactly like the ones a
/// resolution returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyAddress {
    group: String,
    version: String,
    family: String,
}

impl FamilyAddress {
    /// Validates and builds a family address from its three parts.
    pub fn new(
        group: impl Into<String>,
        version: impl Into<String>,
        family: impl Into<String>,
    ) -> Result<Self, ToolError> {
        let (group, version, family) = (group.into(), version.into(), family.into());

        validate("group", &group, &GROUP_RE)?;
        validate("version", &version, &VERSION_RE)?;
        validate("family", &family, &FAMILY_RE)?;

        Ok(Self {
            group,
            version,
            family,
        })
    }

    /// The API group.
    pub fn group(&self) -> &str {
        &self.group
    }

    /// The served version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The family — the type's `spec.names.plural`.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// The URL path segments of the family's collection.
    pub fn segments(&self) -> [&str; 4] {
        [&self.group, &self.version, "items", &self.family]
    }
}

impl std::fmt::Display for FamilyAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/{}/items/{}", self.group, self.version, self.family)
    }
}

/// Where an Item Type Definition lives: `/mia-platform.eu/v1/item-type-definitions/<plural>.<group>`.
///
/// The name is **derived**, never taken as given: the engine requires `metadata.name` to equal
/// `<spec.names.plural>.<spec.group>` (`upsert_itd`), so an address built from the two parts cannot
/// disagree with the body it is written with. Both parts are validated against the engine's own
/// patterns, as every other address is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTypeAddress {
    group: String,
    plural: String,
    name: String,
}

impl ItemTypeAddress {
    /// The core API group every Item Type Definition is served under.
    pub const API_GROUP: &str = "mia-platform.eu";

    /// The core API version every Item Type Definition is served under.
    pub const API_VERSION: &str = "v1";

    /// The collection every Item Type Definition belongs to.
    pub const FAMILY: &str = "item-type-definitions";

    /// The `kind` of an Item Type Definition document itself.
    pub const KIND: &str = "ItemTypeDefinition";

    /// Validates and builds the address of the type whose items are `plural` in `group`.
    pub fn new(group: impl Into<String>, plural: impl Into<String>) -> Result<Self, ToolError> {
        let (group, plural) = (group.into(), plural.into());

        validate("group", &group, &GROUP_RE)?;
        validate("plural", &plural, &FAMILY_RE)?;

        let name = format!("{plural}.{group}");

        Ok(Self {
            group,
            plural,
            name,
        })
    }

    /// The type's API group, `spec.group`.
    pub fn group(&self) -> &str {
        &self.group
    }

    /// The type's family, `spec.names.plural`.
    pub fn plural(&self) -> &str {
        &self.plural
    }

    /// The definition's own name, `<plural>.<group>`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `mia-platform.eu/v1`, the definition document's own `apiVersion`.
    pub fn api_version() -> String {
        format!("{}/{}", Self::API_GROUP, Self::API_VERSION)
    }

    /// The URL path segments of the definition.
    pub fn segments(&self) -> [&str; 4] {
        [Self::API_GROUP, Self::API_VERSION, Self::FAMILY, &self.name]
    }
}

impl std::fmt::Display for ItemTypeAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "/{}/{}/{}/{}",
            Self::API_GROUP,
            Self::API_VERSION,
            Self::FAMILY,
            self.name
        )
    }
}

/// Whether `group` is an API group the engine accepts (its `spec.group` pattern) — for a tool
/// narrowing a shared `kind` to one type (DR-80).
pub fn is_valid_group(group: &str) -> bool {
    group.len() <= MAX_SEGMENT_LENGTH && GROUP_RE.is_match(group)
}

/// Whether `name` is an object name the engine accepts (its `metadata.name` pattern) — for a tool
/// that must refuse a malformed name before spending a `kind` lookup on it (T8 §3).
pub fn is_valid_name(name: &str) -> bool {
    name.len() <= MAX_SEGMENT_LENGTH && NAME_RE.is_match(name)
}

/// Validates one path segment against the engine's own pattern for it.
fn validate(field: &'static str, value: &str, pattern: &Regex) -> Result<(), ToolError> {
    if value.is_empty() {
        return Err(segment_error(field, value, "must not be empty"));
    }

    if value.len() > MAX_SEGMENT_LENGTH {
        return Err(segment_error(
            field,
            value,
            &format!("must be at most {MAX_SEGMENT_LENGTH} characters"),
        ));
    }

    if !pattern.is_match(value) {
        return Err(segment_error(
            field,
            value,
            &format!("must match `{}`", pattern.as_str()),
        ));
    }

    Ok(())
}

/// The `invalid_input` error for a malformed segment — the same code the engine's own `400`
/// maps to, deliberately: same fix, same words, whether we or the engine caught it.
fn segment_error(field: &'static str, value: &str, reason: &str) -> ToolError {
    ToolError::new(
        codes::INVALID_INPUT,
        Remedy::RetryAfterChange,
        format!("`{field}` {reason}."),
    )
    .with_details(serde_json::json!({ "field": field, "value": value }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::plain("example-item", true)]
    #[case::dotted("example.item-2", true)]
    #[case::uppercase("Example-Item", false)]
    #[case::leading_dash("-example", false)]
    #[case::trailing_dot("example.", false)]
    #[case::empty("", false)]
    fn test_a_name_is_checked_against_the_engine_pattern(#[case] name: &str, #[case] valid: bool) {
        assert_eq!(is_valid_name(name), valid);
    }

    #[rstest]
    fn test_an_item_type_address_derives_its_name() {
        let address = ItemTypeAddress::new("stable.example.com", "services")
            .expect("a well-formed type address");

        assert_eq!(address.name(), "services.stable.example.com");
        assert_eq!(
            address.to_string(),
            "/mia-platform.eu/v1/item-type-definitions/services.stable.example.com"
        );
    }

    #[rstest]
    #[case::bad_group("Not A Group", "services")]
    #[case::bad_plural("stable.example.com", "Services")]
    #[case::dotted_plural("stable.example.com", "my.services")]
    fn test_an_item_type_address_is_validated(#[case] group: &str, #[case] plural: &str) {
        assert_eq!(
            ItemTypeAddress::new(group, plural).map_err(|error| error.code),
            Err(codes::INVALID_INPUT)
        );
    }

    #[rstest]
    fn test_a_well_formed_address_is_accepted() {
        let address = ItemAddress::new("stable.example.com", "v1", "services", "example-item")
            .expect("a well-formed address");

        assert_eq!(address.api_version(), "stable.example.com/v1");
        assert_eq!(
            address.to_string(),
            "/stable.example.com/v1/items/services/example-item"
        );
        assert_eq!(
            address.segments(),
            [
                "stable.example.com",
                "v1",
                "items",
                "services",
                "example-item"
            ]
        );
    }

    /// A malformed segment is a tool error before a request is built, not a `400` round trip.
    #[rstest]
    #[case::uppercase_name("stable.example.com", "v1", "services", "Example-Item")]
    #[case::name_with_slash("stable.example.com", "v1", "services", "a/b")]
    #[case::name_with_traversal("stable.example.com", "v1", "services", "..")]
    #[case::empty_name("stable.example.com", "v1", "services", "")]
    #[case::family_with_dot("stable.example.com", "v1", "my.services", "example-item")]
    #[case::bad_version("stable.example.com", "1", "services", "example-item")]
    #[case::bad_version_suffix("stable.example.com", "v1gamma1", "services", "example-item")]
    #[case::group_with_underscore("stable_example.com", "v1", "services", "example-item")]
    fn test_a_malformed_segment_is_refused(
        #[case] group: &str,
        #[case] version: &str,
        #[case] family: &str,
        #[case] name: &str,
    ) {
        let error = ItemAddress::new(group, version, family, name)
            .expect_err("a malformed segment is refused");

        assert_eq!(error.code, codes::INVALID_INPUT);
        assert_eq!(error.remedy, Remedy::RetryAfterChange);
    }

    #[rstest]
    #[case::alpha("v1alpha1")]
    #[case::beta("v2beta1")]
    #[case::plain("v10")]
    fn test_prerelease_versions_are_accepted(#[case] version: &str) {
        assert!(
            ItemAddress::new("stable.example.com", version, "services", "example-item").is_ok()
        );
    }

    #[rstest]
    fn test_an_over_long_segment_is_refused() {
        let name = "a".repeat(MAX_SEGMENT_LENGTH + 1);

        let error = ItemAddress::new("stable.example.com", "v1", "services", &name)
            .expect_err("an over-long name is refused");

        assert!(error.message.contains("253"));
    }

    #[rstest]
    fn test_an_address_is_built_from_a_manifest() {
        let address =
            ItemAddress::from_manifest("stable.example.com/v1", Some("services"), "example-item")
                .expect("a well-formed manifest");

        assert_eq!(address.group(), "stable.example.com");
        assert_eq!(address.version(), "v1");
        assert_eq!(address.family(), "services");
        assert_eq!(address.name(), "example-item");
    }

    /// D30 — `metadata.family == null` is a real engine state, reported as such, never as an
    /// empty result and never as a lookup miss.
    #[rstest]
    fn test_a_null_family_is_an_explicit_unaddressable_item() {
        let error = ItemAddress::from_manifest("stable.example.com/v1", None, "example-item")
            .expect_err("a null family is unaddressable");

        assert_eq!(error.code, codes::UNADDRESSABLE_ITEM);
        assert_eq!(error.remedy, Remedy::Escalate);
        assert!(error.message.contains("item type no longer exists"));
    }

    #[rstest]
    fn test_an_api_version_without_a_slash_is_refused() {
        let error = ItemAddress::from_manifest("stable.example.com", Some("services"), "x")
            .expect_err("a malformed apiVersion is refused");

        assert_eq!(error.code, codes::INVALID_INPUT);
    }

    /// A family address is validated like an item address, so coordinates that come back in a
    /// cursor are checked before they become a path.
    #[rstest::rstest]
    #[case::bad_group("Not A Group", "v1", "services")]
    #[case::bad_version("mia-platform.eu", "1", "services")]
    #[case::bad_family("mia-platform.eu", "v1", "../items")]
    fn test_a_family_address_refuses_a_malformed_segment(
        #[case] group: &str,
        #[case] version: &str,
        #[case] family: &str,
    ) {
        let error =
            FamilyAddress::new(group, version, family).expect_err("a malformed segment is refused");

        assert_eq!(error.code, codes::INVALID_INPUT);
    }

    #[rstest::rstest]
    fn test_a_family_address_is_the_collection_path() {
        let family =
            FamilyAddress::new("mia-platform.eu", "v1", "services").expect("a well-formed family");

        assert_eq!(
            family.segments(),
            ["mia-platform.eu", "v1", "items", "services"]
        );
        assert_eq!(family.to_string(), "/mia-platform.eu/v1/items/services");
    }
}
