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
    EngineClient, FieldPath, ItemAddress, Predicate, QueryValue, RegexLiteral, Remedy, ToolError,
    error::codes, models::PartialObjectMetadata, ops::ListQuery, resolve_kind_or_suggest,
};
use serde::Serialize;
use serde_json::json;

/// The kindless name probe's page: **two**, because one row cannot tell *"unique"* from
/// *"the first of several"*, and two is all it takes to know which.
const NAME_PROBE_LIMIT: u32 = 2;

/// How many candidates an ambiguous name is answered with, fetched only on that branch.
pub(crate) const MAX_AMBIGUOUS_CANDIDATES: u32 = 10;

/// How many near matches a name that matches nothing is answered with.
pub(crate) const MAX_NEAR_MATCHES: u32 = 5;

/// The field the kindless lookup matches on.
const NAME_FIELD: &str = "metadata.name";

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

/// Where the item lives: from `kind` through the core's point lookup, or from the name alone.
///
/// `tool` is the calling tool's name, for the next step an ambiguous name is answered with.
pub(crate) async fn resolve(
    engine: &EngineClient,
    tool: &str,
    name: &str,
    kind: Option<&str>,
    group: Option<&str>,
) -> Result<ItemAddress, ToolError> {
    match kind {
        // An unknown `kind` is answered with near matches, and a shared one with its candidates,
        // by the core.
        Some(kind) => {
            let coordinates = resolve_kind_or_suggest(engine, kind, group).await?;

            ItemAddress::new(
                &coordinates.group,
                &coordinates.version,
                &coordinates.family,
                name,
            )
        }
        None => resolve_by_name(engine, tool, name).await,
    }
}

/// The kindless path: a two-row probe on `metadata.name`.
///
/// One row is the item. Two are an ambiguity, **never** a guess — silently picking one would have
/// the model act confidently against the wrong item — so a wider fetch collects the candidates, on
/// that branch only. None is `not_found`, with near matches when a substring search finds any.
async fn resolve_by_name(
    engine: &EngineClient,
    tool: &str,
    name: &str,
) -> Result<ItemAddress, ToolError> {
    let exact = Predicate::Eq {
        field: FieldPath::new(NAME_FIELD)?,
        value: QueryValue::string(name)?,
    };
    let rows = list_by(engine, &exact, NAME_PROBE_LIMIT).await?;

    match rows.as_slice() {
        [only] => ItemAddress::from_manifest(
            &only.api_version,
            only.metadata.family.as_deref(),
            &only.metadata.name,
        ),
        [] => Err(no_such_item(engine, name).await),
        _ => {
            let candidates = match list_by(engine, &exact, MAX_AMBIGUOUS_CANDIDATES).await {
                Ok(wider) if wider.len() >= rows.len() => wider,
                _ => rows,
            };

            Err(ToolError::new(
                codes::NOT_FOUND,
                Remedy::RetryAfterChange,
                format!(
                    "`{name}` names more than one item. Say which with `kind`; the candidates are \
                     listed."
                ),
            )
            .with_details(json!({
                "name": name,
                "candidates": candidates.into_iter().map(candidate).collect::<Vec<_>>(),
            }))
            .with_next_step(format!(
                "call {tool} again with `kind` set to the intended candidate's"
            )))
        }
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
