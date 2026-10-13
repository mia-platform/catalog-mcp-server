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
// `search_catalog`'s filters — free text, type, labels and fields — as one unit: their bounds, the
// type they name, and the predicate they become. Moved here unchanged from `search_catalog`, so a
// compliance scope can be written with exactly the same filters and checked by exactly the same
// rules. The parameters → `Predicate` mapping is the whole contribution to the query translator;
// encoding, limits, splitting and base64 are `catalog-client`'s.

use crate::tools::arguments::validate_group;
use catalog_client::{
    EngineClient, FamilyAddress, FieldPath, Predicate, QueryValue, RegexLiteral, Remedy, ToolError,
    TypeCoordinates,
    error::codes,
    is_valid_kind,
    query::{MAX_BRANCH_CHILDREN, MAX_VALUE_BYTES, OWNER_FIELD, is_valid_label_key},
    resolve_kind_or_suggest,
};
use serde_json::json;
use std::collections::BTreeMap;

/// The group of the relationships family.
pub const RELATIONSHIP_GROUP: &str = "mia-platform.eu";

/// The relationships family, whose rows carry what each relationship connects.
pub const RELATIONSHIP_FAMILY: &str = "relationships";

/// The families of the relationships group that the catalog stores in tables of their own, with no
/// owner column: their records never have an owner, and a filter on one would reach a column that
/// does not exist.
const UNOWNED_FAMILIES: [&str; 4] = [
    RELATIONSHIP_FAMILY,
    "relationship-types",
    "relationship-constraints",
    "custom-fields",
];

/// What a kind-less filter on a `spec.` path is answered with.
///
/// Without `kind` the catalog checks the path against each type's *filterable* fields only, so an
/// item of a type that has the field but does not declare it filterable is silently left out —
/// the result can be incomplete with nothing in it saying so. With `kind`, a path the type does not
/// declare is refused instead, which is why adding it is the remedy.
pub const SPEC_WITHOUT_KIND_WARNING: &str = "Without `kind`, a `spec.` filter matches only types \
     that declare that field filterable, so items of other types may be missing. Add `kind` for a \
     complete answer.";

/// The next step of a `spec.` path a `Relationship` filter cannot use.
const RELATIONSHIP_FIELDS_NEXT_STEP: &str =
    "filter `spec.sourceRef` or `spec.targetRef` with the item's `urn`, from describe_item";

/// The longest `query`, in bytes, **before** escaping.
///
/// The translator caps the escaped literal at the same number, and escaping can double a
/// metacharacter-dense string, so this is checked first and the error says which cap was hit.
pub const MAX_QUERY_BYTES: usize = 256;

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// The most entries `labels` or `fields` may carry.
pub const MAX_FILTER_ENTRIES: usize = 20;

/// The prefix of a field path the family-scoped endpoint restricts to its type's selectable
/// fields. `metadata.*` paths are accepted on both endpoints.
pub const SPEC_PATH_PREFIX: &str = "spec.";

/// The fields free text is matched against, in the order they are emitted.
///
/// `metadata.tags` is an array, and the engine matches a pattern against **any element** of it
/// (`EXISTS … unnest(…) … ~*`) — which is what lets one `matches` cover a tag list.
const QUERY_FIELDS: [&str; 3] = ["metadata.name", "metadata.title", "metadata.tags"];

/// The `fields` argument: a path, and the value it must equal — or `None`, for a field that must
/// be unset.
pub type FieldFilters = BTreeMap<String, Option<String>>;

/// The field-path prefix a label key is appended to.
pub const LABEL_PATH_PREFIX: &str = "metadata.labels.";

/// The filters, borrowed from whichever tool's input carries them.
pub struct Filters<'a> {
    /// Free text over names, titles and tags.
    pub query: Option<&'a str>,

    /// One type.
    pub kind: Option<&'a str>,

    /// The kind's group, if several types share it.
    pub group: Option<&'a str>,

    /// Exact-match label filters.
    pub labels: Option<&'a BTreeMap<String, String>>,

    /// Exact-match field filters; `None` for an unset field.
    pub fields: Option<&'a FieldFilters>,
}

/// A type named by `kind`, resolved: its coordinates and the address of its items.
pub struct ResolvedType {
    /// Group, version, kind and selectable fields, as the type definition states them.
    pub coordinates: TypeCoordinates,

    /// Where the type's items are listed.
    pub family: FamilyAddress,
}

/// Every filter bound, checked before anything reaches the engine.
pub fn validate(filters: &Filters<'_>) -> Result<(), ToolError> {
    if let Some(query) = filters.query
        && query.len() > MAX_QUERY_BYTES
    {
        return Err(invalid(
            "query",
            format!(
                "`query` is {} bytes; at most {MAX_QUERY_BYTES} are accepted before escaping.",
                query.len()
            ),
        ));
    }

    if let Some(kind) = filters.kind {
        if kind.len() > MAX_KIND_BYTES {
            return Err(invalid(
                "kind",
                format!(
                    "`kind` is {} bytes; at most {MAX_KIND_BYTES} are accepted.",
                    kind.len()
                ),
            ));
        }

        if !is_valid_kind(kind) {
            return Err(invalid(
                "kind",
                format!(
                    "`{kind}` is not a kind: a kind is a letter followed by letters and digits."
                ),
            )
            .with_next_step("call list_catalog_types to see the kinds that exist"));
        }
    }

    validate_group(filters.group, filters.kind.is_some())?;

    if let Some(labels) = filters.labels {
        validate_entries(
            "labels",
            labels
                .iter()
                .map(|(key, value)| (key, Some(value.as_str()))),
        )?;

        if let Some(key) = labels.keys().find(|key| !is_valid_label_key(key)) {
            return Err(invalid(
                "labels",
                format!("`{key}` is not a valid label key."),
            ));
        }
    }

    if let Some(fields) = filters.fields {
        validate_entries(
            "fields",
            fields.iter().map(|(path, value)| (path, value.as_deref())),
        )?;
        validate_unset(fields, filters.kind.is_some())?;
    }

    Ok(())
}

/// The entry-count and value-length bounds shared by `labels` and `fields`; a `fields` value is
/// `None` when it asks for an unset field.
fn validate_entries<'a>(
    parameter: &str,
    mut entries: impl ExactSizeIterator<Item = (&'a String, Option<&'a str>)>,
) -> Result<(), ToolError> {
    if entries.len() > MAX_FILTER_ENTRIES {
        return Err(invalid(
            parameter,
            format!(
                "`{parameter}` has {} entries; at most {MAX_FILTER_ENTRIES} are accepted.",
                entries.len()
            ),
        ));
    }

    if let Some(key) = entries.find_map(|(key, value)| {
        value
            .is_some_and(|value| value.len() > MAX_VALUE_BYTES)
            .then_some(key)
    }) {
        return Err(invalid(
            parameter,
            format!("The value of `{parameter}.{key}` is longer than {MAX_VALUE_BYTES} bytes."),
        ));
    }

    Ok(())
}

/// Where a `null` in `fields` — *"this field is unset"* — is accepted.
///
/// On `metadata.owner` always: it is how *"items without an owner"* is asked. On a `spec.` path
/// only with `kind`: the type's own listing knows which fields it can filter, but without `kind`
/// the catalog checks an unset `spec.` field against every type, and every type that does not
/// declare it matches — the whole catalog, not the items that leave it unset. Nowhere else: the
/// other filterable fields are always set, or are not something a caller asks to be absent.
fn validate_unset(fields: &FieldFilters, has_kind: bool) -> Result<(), ToolError> {
    let Some(path) = fields
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(path, _)| path)
        .find(|path| {
            path.as_str() != OWNER_FIELD && !(has_kind && path.starts_with(SPEC_PATH_PREFIX))
        })
    else {
        return Ok(());
    };

    let message = if path.starts_with(SPEC_PATH_PREFIX) {
        format!(
            "`null` on `{path}` needs `kind`: without it, every item whose type does not declare \
             the field would match."
        )
    } else {
        format!(
            "`null` on `{path}` is not accepted: `null` matches an unset field only on \
             `{OWNER_FIELD}` and, with `kind`, on `spec.` fields."
        )
    };

    Err(invalid("fields", message))
}

/// An `invalid_input` naming the offending parameter.
pub fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// Resolves `kind` to its family and checks `fields` against what that family can filter on.
///
/// An unknown `kind` is answered with near matches by `catalog-client`.
pub async fn resolve_family(
    engine: &EngineClient,
    kind: &str,
    group: Option<&str>,
    fields: Option<&FieldFilters>,
) -> Result<FamilyAddress, ToolError> {
    resolve_type(engine, kind, group, fields)
        .await
        .map(|resolved| resolved.family)
}

/// Resolves `kind` to its type, and checks `fields` against what that type can filter on.
///
/// What [`resolve_family`] does, keeping the coordinates as well, for a caller that must name the
/// type in a query of its own rather than select its endpoint.
pub async fn resolve_type(
    engine: &EngineClient,
    kind: &str,
    group: Option<&str>,
    fields: Option<&FieldFilters>,
) -> Result<ResolvedType, ToolError> {
    let coordinates = resolve_kind_or_suggest(engine, kind, group).await?;
    let family = coordinates.family_address()?;

    validate_fields_for(kind, fields, &coordinates.selectable_fields).map_err(|error| {
        if is_relationships(&family) {
            error.with_next_step(RELATIONSHIP_FIELDS_NEXT_STEP)
        } else {
            error
        }
    })?;

    if fields.is_some_and(|fields| fields.contains_key(OWNER_FIELD)) && is_unowned(&family) {
        return Err(invalid(
            "fields",
            format!("`{kind}` items have no owner, so they cannot be filtered on `{OWNER_FIELD}`."),
        ));
    }

    Ok(ResolvedType {
        coordinates,
        family,
    })
}

/// On the `kind` path a `spec.` filter must be one of the type's selectable fields.
///
/// The global endpoint accepts any `spec.` path, so a filter that works without `kind` can be
/// refused with it. That asymmetry is the engine's; naming the valid paths is what stops it
/// being baffling. `metadata.*` paths are accepted on both endpoints.
fn validate_fields_for(
    kind: &str,
    fields: Option<&FieldFilters>,
    selectable: &[String],
) -> Result<(), ToolError> {
    let Some(path) = fields
        .into_iter()
        .flat_map(BTreeMap::keys)
        .find(|path| path.starts_with(SPEC_PATH_PREFIX) && !selectable.contains(path))
    else {
        return Ok(());
    };

    Err(ToolError::new(
        codes::INVALID_INPUT,
        Remedy::RetryAfterChange,
        format!("`{path}` is not a field `{kind}` items can be filtered by."),
    )
    .with_details(json!({ "field": path, "validPaths": selectable })))
}

/// Whether `fields` filters on any `spec.` path.
pub fn filters_on_spec(fields: Option<&FieldFilters>) -> bool {
    fields
        .into_iter()
        .flat_map(BTreeMap::keys)
        .any(|path| path.starts_with(SPEC_PATH_PREFIX))
}

/// Whether `family` is the relationships family, whose rows carry their ends.
pub fn is_relationships(family: &FamilyAddress) -> bool {
    family.group() == RELATIONSHIP_GROUP && family.family() == RELATIONSHIP_FAMILY
}

/// Whether `family` is one whose records never have an owner.
fn is_unowned(family: &FamilyAddress) -> bool {
    family.group() == RELATIONSHIP_GROUP && UNOWNED_FAMILIES.contains(&family.family())
}

/// Builds the filters' predicate, or `None` when nothing was asked for.
///
/// - `query` → an `or` of three `matches`, each with the **same** literal: `regex::escape`d and
///   wrapped `/…/i`, so the semantics are a case-insensitive substring, which is what a person
///   means by "search".
/// - each label → `eq` on `metadata.labels.<key>`; each field → `eq` on its path, or, for a
///   `null` value, `exists: false` on it.
/// - everything is `and`-ed at the top level, which is also the only shape `catalog-client` may
///   split across several `rawq` parameters.
/// - **when that `and` would be wider than `catalog-client`'s [`MAX_BRANCH_CHILDREN`]**, the `eq`s
///   are grouped into nested `and`s of at most that many, after the query's `or`. `query` plus 20
///   labels plus 20 fields — all within the tool's own bounds — would otherwise be refused for
///   width. A conjunction of conjunctions is the same search, and the top-level `and` stays
///   splittable. A search that fits keeps exactly the shape it always had, so its `rawq` and its
///   cursor fingerprint do not change.
///
/// **Nothing supplied is not an error**: it yields `None`, and no `rawq` is sent at all — *"what
/// is in the catalog?"* is a legitimate first question. `kind` is never a predicate here: for a
/// search it selects the endpoint, and repeating it in every `rawq` would only cost bytes.
///
/// `BTreeMap` iteration makes the result independent of the order the model wrote its keys in,
/// so two identical searches produce byte-identical `rawq` and the same cursor fingerprint.
pub fn build(
    query: Option<&str>,
    labels: Option<&BTreeMap<String, String>>,
    fields: Option<&FieldFilters>,
) -> Result<Option<Predicate>, ToolError> {
    let mut clauses = Vec::new();
    let mut equalities = Vec::new();

    if let Some(query) = query {
        let pattern = RegexLiteral::containing(query)?;
        let matches = QUERY_FIELDS
            .into_iter()
            .map(|field| {
                Ok(Predicate::Matches {
                    field: FieldPath::new(field)?,
                    pattern: pattern.clone(),
                })
            })
            .collect::<Result<Vec<_>, ToolError>>()?;

        clauses.push(Predicate::Or(matches));
    }

    for (key, value) in labels.into_iter().flatten() {
        equalities.push(Predicate::Eq {
            field: FieldPath::new(&format!("{LABEL_PATH_PREFIX}{key}"))?,
            value: QueryValue::string(value)?,
        });
    }

    for (path, value) in fields.into_iter().flatten() {
        let field = FieldPath::new(path)?;
        equalities.push(match value {
            Some(value) => Predicate::Eq {
                field,
                value: QueryValue::string(value)?,
            },
            None => Predicate::Missing { field },
        });
    }

    if clauses.len() + equalities.len() <= MAX_BRANCH_CHILDREN {
        clauses.extend(equalities);
    } else {
        clauses.extend(
            equalities
                .chunks(MAX_BRANCH_CHILDREN)
                .map(|group| match group {
                    [only] => only.clone(),
                    _ => Predicate::And(group.to_vec()),
                }),
        );
    }

    Ok((!clauses.is_empty()).then_some(Predicate::And(clauses)))
}
