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
// Finding the item a tool was given by name, shared by every tool that addresses one item:
// `describe_item` reads it, `delete_item` destroys it. One implementation, because
// the rule it enforces — **never guess** — must mean the same on a read and on a delete.

use catalog_client::{
    EngineClient, FieldPath, ItemAddress, KindResolution, Predicate, QueryValue, RegexLiteral,
    Remedy, ToolError, error::codes, models::PartialObjectMetadata, ops::ListQuery,
    resolve_kind_or_shared,
};
use serde::Serialize;
use serde_json::json;

/// The name probe's page: **two**, because one row cannot tell *"unique"* from *"the first of
/// several"*, and two is all it takes to know which.
const NAME_PROBE_LIMIT: u32 = 2;

/// How many candidates an ambiguous name is answered with, fetched only on that branch.
pub(crate) const MAX_AMBIGUOUS_CANDIDATES: u32 = 10;

/// How many near matches a name that matches nothing is answered with.
pub(crate) const MAX_NEAR_MATCHES: u32 = 5;

/// The field the name probe matches on.
const NAME_FIELD: &str = "metadata.name";

/// The field a probe narrowed to one kind matches on.
const KIND_FIELD: &str = "kind";

/// The field a probe narrowed to one group matches on: `<group>/<version>`.
const API_VERSION_FIELD: &str = "apiVersion";

/// One candidate for an ambiguous or unknown name.
#[derive(Serialize)]
struct Candidate {
    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "version", skip_serializing_if = "Option::is_none")]
    version: Option<String>,

    #[serde(rename = "family", skip_serializing_if = "Option::is_none")]
    family: Option<String>,
}

/// Where the item is, and — when finding it took more than the arguments said — a note saying
/// how, for the tool to pass on as a warning.
pub(crate) struct Resolved {
    /// The item's address.
    pub address: ItemAddress,

    /// How a half-given type was completed, when it was.
    pub note: Option<String>,
}

/// What the name probe is narrowed to.
#[derive(Clone, Copy)]
enum Scope<'a> {
    /// Every item: no `kind`, no `group`.
    Anywhere,

    /// Items of a `kind` several groups share, given without the `group`.
    SharedKind(&'a str),

    /// Items of a `group`, given without the `kind`.
    Group(&'a str),
}

/// Where the item lives: from `kind` (and `group`) through `catalog-client`'s point lookup, or
/// from the name.
///
/// The name decides whenever the type is only partly given, the same way it does when the type
/// is not given at all: a shared `kind` without its `group`, or a `group` without a `kind`,
/// narrows the name probe instead of failing. One row is the item, with a note saying how the
/// type was completed; several are candidates; none is `not_found` — never a pick.
///
/// `tool` is the calling tool's name, for the next step an ambiguous name is answered with.
pub(crate) async fn resolve(
    engine: &EngineClient,
    tool: &str,
    name: &str,
    kind: Option<&str>,
    group: Option<&str>,
) -> Result<Resolved, ToolError> {
    let scope = match (kind, group) {
        // An unknown `kind` is answered with near matches by `catalog-client`.
        (Some(kind), group) => match resolve_kind_or_shared(engine, kind, group).await? {
            KindResolution::One(coordinates) => {
                return Ok(Resolved {
                    address: ItemAddress::new(
                        &coordinates.group,
                        &coordinates.version,
                        &coordinates.family,
                        name,
                    )?,
                    note: None,
                });
            }
            KindResolution::Shared(_) => Scope::SharedKind(kind),
        },
        (None, Some(group)) => Scope::Group(group),
        (None, None) => Scope::Anywhere,
    };

    resolve_by_name(engine, tool, name, scope).await
}

/// The name probe, two rows, narrowed to `scope`.
///
/// One row is the item. Two are an ambiguity, **never** a guess — silently picking one would have
/// the model act confidently against the wrong item — so a wider fetch collects the candidates, on
/// that branch only. None is `not_found`, with near matches when a substring search finds any.
async fn resolve_by_name(
    engine: &EngineClient,
    tool: &str,
    name: &str,
    scope: Scope<'_>,
) -> Result<Resolved, ToolError> {
    let probe = probe(name, scope)?;
    let rows = list_by(engine, &probe, NAME_PROBE_LIMIT).await?;

    match rows.as_slice() {
        [only] => Ok(Resolved {
            address: ItemAddress::from_manifest(
                &only.api_version,
                only.metadata.family.as_deref(),
                &only.metadata.name,
            )?,
            note: note(name, scope, only),
        }),
        [] => Err(no_such_item(engine, name).await),
        _ => {
            let candidates = match list_by(engine, &probe, MAX_AMBIGUOUS_CANDIDATES).await {
                Ok(wider) if wider.len() >= rows.len() => wider,
                _ => rows,
            };
            let (message, missing) = match scope {
                Scope::Anywhere => (format!("`{name}` names more than one item."), "kind"),
                Scope::SharedKind(kind) => (
                    format!("`{name}` names a `{kind}` in more than one group."),
                    "group",
                ),
                Scope::Group(group) => (
                    format!("`{name}` names more than one item in `{group}`."),
                    "kind",
                ),
            };

            Err(ToolError::new(
                codes::NOT_FOUND,
                Remedy::RetryAfterChange,
                format!("{message} Say which with `{missing}`; the candidates are listed."),
            )
            .with_details(json!({
                "name": name,
                "candidates": candidates.into_iter().map(candidate).collect::<Vec<_>>(),
            }))
            .with_next_step(format!(
                "call {tool} again with `{missing}` set to the intended candidate's"
            )))
        }
    }
}

/// `metadata.name` equal to `name`, and — when the type was partly given — the part that was.
fn probe(name: &str, scope: Scope<'_>) -> Result<Predicate, ToolError> {
    let exact = Predicate::Eq {
        field: FieldPath::new(NAME_FIELD)?,
        value: QueryValue::string(name)?,
    };
    let narrowing = match scope {
        Scope::Anywhere => return Ok(exact),
        Scope::SharedKind(kind) => Predicate::Eq {
            field: FieldPath::new(KIND_FIELD)?,
            value: QueryValue::string(kind)?,
        },
        // `<group>/` anchored at the start: a group is matched whole, never as the tail of a
        // longer one that ends with the same text.
        Scope::Group(group) => Predicate::Matches {
            field: FieldPath::new(API_VERSION_FIELD)?,
            pattern: RegexLiteral::prefix(&format!("{group}/"))?,
        },
    };

    Ok(Predicate::And(vec![exact, narrowing]))
}

/// How the type was completed, for a probe that was narrowed by half of it.
fn note(name: &str, scope: Scope<'_>, found: &PartialObjectMetadata) -> Option<String> {
    let group = found
        .api_version
        .split_once('/')
        .map_or(found.api_version.as_str(), |(group, _)| group);

    match scope {
        Scope::Anywhere => None,
        Scope::SharedKind(kind) => Some(format!(
            "`{kind}` is shared by several groups; resolved to `{group}`, the only one with an item \
             named `{name}`."
        )),
        Scope::Group(group) => Some(format!(
            "Resolved to `{}`, the only type in `{group}` with an item named `{name}`.",
            found.kind
        )),
    }
}

/// The `not_found` for a name that matches no item, with near matches when a substring search
/// finds any — also what `delete_item` answers when its pre-read finds nothing.
pub(crate) async fn no_such_item(engine: &EngineClient, name: &str) -> ToolError {
    let error = ToolError::new(
        codes::NOT_FOUND,
        Remedy::RetryAfterChange,
        format!("No item named `{name}` exists in this tenant."),
    )
    .with_next_step("call search_catalog to find the item by a partial name or its type");

    // The error path's extra call: if it fails, the plain `not_found` still stands.
    let candidates = match near_pattern(name) {
        Ok(near) => list_by(engine, &near, MAX_NEAR_MATCHES)
            .await
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    if candidates.is_empty() {
        error.with_details(json!({ "name": name }))
    } else {
        error.with_details(json!({
            "name": name,
            "candidates": candidates.into_iter().map(candidate).collect::<Vec<_>>(),
        }))
    }
}

/// The substring match on `metadata.name` the near matches come from.
fn near_pattern(name: &str) -> Result<Predicate, ToolError> {
    Ok(Predicate::Matches {
        field: FieldPath::new(NAME_FIELD)?,
        pattern: RegexLiteral::containing(name)?,
    })
}

/// One global, metadata-only listing filtered by `predicate`.
async fn list_by(
    engine: &EngineClient,
    predicate: &Predicate,
    limit: u32,
) -> Result<Vec<PartialObjectMetadata>, ToolError> {
    let page = engine
        .list_items_partial(&ListQuery {
            limit: Some(limit),
            raw_query: predicate.encode_rawq()?,
            ..ListQuery::default()
        })
        .await?;

    Ok(page.value.items)
}

/// A listed item as a candidate.
fn candidate(item: PartialObjectMetadata) -> Candidate {
    let (group, version) = match item.api_version.split_once('/') {
        Some((group, version)) => (group.to_string(), Some(version.to_string())),
        None => (item.api_version, None),
    };

    Candidate {
        name: item.metadata.name,
        kind: item.kind,
        group,
        version,
        family: item.metadata.family,
    }
}
