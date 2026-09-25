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
use crate::registry::{
    ToolDescriptor,
    contract::{CallContext, Tool, ToolOutput},
};
use catalog_client::{
    EngineClient, FamilyAddress, Predicate, Remedy, ToolError,
    error::codes,
    is_valid_kind,
    models::PartialObjectMetadata,
    ops::ListQuery,
    pagination::{DEFAULT_LIMIT, MAX_LIMIT},
    query::{MAX_VALUE_BYTES, is_valid_label_key},
    resolve_kind_or_suggest,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// T2 §4 — parameters to the translator's AST.
mod ast;

/// T2 §6 — what a search cursor pins, and its fingerprint.
mod cursor;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "search_catalog";

/// What the tool does (T2 §3.2). No `rawq`, no base64, no coordinates, no pagination mechanics.
const TOOL_DESCRIPTION: &str = "Searches the catalog. Use `query` for free text over names, \
     titles and tags; `kind` to restrict to one type; `labels` and `fields` to filter exactly. \
     Returns items with everything needed to act on them. Call `list_catalog_types` first if you \
     do not know the exact `kind`.";

/// The longest `query`, in bytes, **before** escaping (T2 §3.1).
///
/// The translator caps the escaped literal at the same number, and escaping can double a
/// metacharacter-dense string, so this is checked first and the error says which cap was hit.
pub const MAX_QUERY_BYTES: usize = 256;

/// The longest `kind`, in bytes.
pub const MAX_KIND_BYTES: usize = 128;

/// The most entries `labels` or `fields` may carry.
pub const MAX_FILTER_ENTRIES: usize = 20;

/// The smallest page. The engine checks only the upper bound, so a `0` would be sent as is.
const MIN_LIMIT: u32 = 1;

/// The prefix of a field path the family-scoped endpoint restricts to its type's selectable
/// fields (T2-D3). `metadata.*` paths are accepted on both endpoints.
const SPEC_PATH_PREFIX: &str = "spec.";

/// Arguments for `search_catalog` (T2 §3.1).
///
/// `BTreeMap` for both maps, so the query — and therefore the cursor fingerprint — does not
/// depend on the order the model emitted its keys in. No `#[serde(default)]`: an absent `Option`
/// is already `None`, and the attribute would put `"default": null` into the schema.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct SearchCatalogInput {
    /// Free text over names, titles and tags.
    #[serde(rename = "query")]
    pub query: Option<String>,

    /// Restrict to one type, e.g. "Service".
    #[serde(rename = "kind")]
    pub kind: Option<String>,

    /// Exact-match label filters.
    #[serde(rename = "labels")]
    pub labels: Option<BTreeMap<String, String>>,

    /// Exact-match field filters, by JSON path.
    #[serde(rename = "fields")]
    pub fields: Option<BTreeMap<String, String>>,

    /// Page size. Default 50, clamped to 200.
    #[serde(rename = "limit")]
    pub limit: Option<u16>,

    /// The cursor a previous call returned, to fetch the next page.
    //
    // Reworded from T2 §3.1's "opaque continuation token": D21's guard refuses the word "token"
    // in any schema, because that is how an identity parameter would reappear.
    #[serde(rename = "cursor")]
    pub cursor: Option<String>,
}

/// One result row (T2 §6). Every field comes from the metadata-only projection, so no second
/// lookup is needed to act on a result.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct SearchRow {
    /// `metadata.name`.
    #[serde(rename = "name")]
    pub name: String,

    /// The item's `kind`, which for an item is its type's.
    #[serde(rename = "kind")]
    pub kind: String,

    /// `metadata.title`, when set.
    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// The group half of `apiVersion`.
    #[serde(rename = "group")]
    pub group: String,

    /// The version half of `apiVersion`.
    #[serde(rename = "version", skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// `metadata.family`. Absent only for an item whose type no longer exists (D30).
    #[serde(rename = "family", skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,

    /// `metadata.labels`, omitted when there are none.
    #[serde(rename = "labels", skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// The whole response (T2 §6).
#[derive(Serialize)]
struct SearchOutput {
    #[serde(rename = "items")]
    items: Vec<SearchRow>,

    /// Every match, counted only when the page was full (T2-D5).
    #[serde(rename = "total")]
    total: u64,

    /// Absent when there are no more results.
    #[serde(rename = "cursor", skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,

    /// The effective page size, **only** when it differs from the one asked for (T2-D6): a
    /// silent clamp would let the model believe it had seen everything.
    #[serde(rename = "limit", skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,

    /// The filters as interpreted, **only** on an empty result, so *"nothing matched"* can be told
    /// from *"I filtered wrongly"*.
    #[serde(rename = "filters", skip_serializing_if = "Option::is_none")]
    filters: Option<Value>,
}

/// T2 · `search_catalog` — find items by free text, type, labels and fields.
///
/// One listing — global, or scoped to a family when `kind` is given — plus a count only when the
/// page is full. The query translator is the core's; this tool builds the AST and chooses the
/// endpoint.
pub struct SearchCatalog;

impl Tool for SearchCatalog {
    type Input = SearchCatalogInput;

    /// `readOnlyHint: true`; every other hint is the specification's default (D16).
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<SearchCatalogInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().read_only(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;

        let (limit, limit_echo) = effective_limit(input.limit);
        let predicate = ast::build(
            input.query.as_deref(),
            input.labels.as_ref(),
            input.fields.as_ref(),
        )?;
        // Built once and used for the listing **and** the count, so the two cannot disagree.
        let raw_query = match &predicate {
            Some(predicate) => predicate.encode_rawq()?,
            None => Vec::new(),
        };
        let fingerprint = cursor::fingerprint(input.kind.as_deref(), predicate.as_ref());
        let engine = context.engine();

        let (family, engine_cursor, returned) = match &input.cursor {
            Some(raw) => {
                let resumed = cursor::resume(raw, &fingerprint, input.kind.is_some())?;
                (resumed.family, Some(resumed.engine), resumed.returned)
            }
            None => {
                let family = match &input.kind {
                    Some(kind) => Some(resolve_family(engine, kind, input.fields.as_ref()).await?),
                    None => None,
                };
                (family, None, 0)
            }
        };

        let rawq_parameters = raw_query.len();
        let query = ListQuery {
            limit: Some(limit),
            cursor: engine_cursor,
            raw_query,
            ..ListQuery::default()
        };

        let page = match &family {
            Some(family) => engine.list_family_items_partial(family, &query).await,
            None => engine.list_items_partial(&query).await,
        }
        .inspect_err(|error| log_if_ours(error, predicate.as_ref()))?
        .value;

        // T2-D5 — a page that is not full already says how many there are.
        let count_fired = page.items.len() as u64 >= u64::from(limit);
        let total = if count_fired {
            match &family {
                Some(family) => engine.count_family_items(family, &query).await,
                None => engine.count_items(&query).await,
            }
            .inspect_err(|error| log_if_ours(error, predicate.as_ref()))?
            .value
            .count
        } else {
            returned + page.items.len() as u64
        };

        // T2 §9 — the inputs to any future tuning. `rawq_parameters` should be 1 in every real
        // search; more means the split rule fired.
        tracing::debug!(count_fired, rawq_parameters, "searched the catalog");

        let returned_now = returned + page.items.len() as u64;
        let next_cursor = page
            .next
            .as_ref()
            .map(|next| cursor::mint(next, &fingerprint, family.as_ref(), returned_now))
            .transpose()?;

        // T2-D7 — the page is emitted whole: the engine's cursor points after what was fetched.
        let items: Vec<SearchRow> = page.items.into_iter().map(project).collect();
        let filters = items.is_empty().then(|| interpreted_filters(&input));

        let output = SearchOutput {
            items,
            total,
            cursor: next_cursor,
            limit: limit_echo,
            filters,
        };

        let payload = serde_json::to_value(&output).map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The search result could not be rendered: {err}"),
            )
        })?;

        Ok(ToolOutput::new(payload))
    }
}

/// NFR-10 — every bound of T2 §3.1, checked before anything reaches the engine.
fn validate(input: &SearchCatalogInput) -> Result<(), ToolError> {
    if let Some(query) = &input.query
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

    if let Some(kind) = &input.kind {
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

    if let Some(labels) = &input.labels {
        validate_entries("labels", labels)?;

        if let Some(key) = labels.keys().find(|key| !is_valid_label_key(key)) {
            return Err(invalid(
                "labels",
                format!("`{key}` is not a valid label key."),
            ));
        }
    }

    if let Some(fields) = &input.fields {
        validate_entries("fields", fields)?;
    }

    Ok(())
}

/// The entry-count and value-length bounds shared by `labels` and `fields`.
fn validate_entries(parameter: &str, entries: &BTreeMap<String, String>) -> Result<(), ToolError> {
    if entries.len() > MAX_FILTER_ENTRIES {
        return Err(invalid(
            parameter,
            format!(
                "`{parameter}` has {} entries; at most {MAX_FILTER_ENTRIES} are accepted.",
                entries.len()
            ),
        ));
    }

    if let Some(key) = entries
        .iter()
        .find_map(|(key, value)| (value.len() > MAX_VALUE_BYTES).then_some(key))
    {
        return Err(invalid(
            parameter,
            format!("The value of `{parameter}.{key}` is longer than {MAX_VALUE_BYTES} bytes."),
        ));
    }

    Ok(())
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

/// The page size to ask for, and what to echo (T2-D6).
///
/// Clamped into the engine's range rather than rejected — it would `400` anything above 200 —
/// and echoed **only** when the clamp changed it.
fn effective_limit(requested: Option<u16>) -> (u32, Option<u32>) {
    let Some(requested) = requested else {
        return (DEFAULT_LIMIT, None);
    };

    let requested = u32::from(requested);
    let effective = requested.clamp(MIN_LIMIT, MAX_LIMIT);

    (effective, (effective != requested).then_some(effective))
}

/// Resolves `kind` to its family and checks `fields` against what that family can filter on.
///
/// An unknown `kind` is answered with near matches by the core (T2-D9).
async fn resolve_family(
    engine: &EngineClient,
    kind: &str,
    fields: Option<&BTreeMap<String, String>>,
) -> Result<FamilyAddress, ToolError> {
    let coordinates = resolve_kind_or_suggest(engine, kind).await?;

    validate_fields_for(kind, fields, &coordinates.selectable_fields)?;

    coordinates.family_address()
}

/// T2-D3 — on the `kind` path a `spec.` filter must be one of the type's selectable fields.
///
/// The global endpoint accepts any `spec.` path, so a filter that works without `kind` can be
/// refused with it. That asymmetry is the engine's; naming the valid paths is what stops it
/// being baffling. `metadata.*` paths are accepted on both endpoints.
fn validate_fields_for(
    kind: &str,
    fields: Option<&BTreeMap<String, String>>,
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

/// T2 §8 — a `400` on a query this server built is logged with the **decoded** query, never the
/// base64: the model is told it is not its fault, and an operator is shown what was sent.
fn log_if_ours(error: &ToolError, predicate: Option<&Predicate>) {
    if error.code == codes::SERVER_DEFECT {
        let decoded = predicate.map_or(Value::Null, Predicate::to_json);

        tracing::error!(
            query = %decoded,
            "the catalog rejected a search this server built"
        );
    }
}

/// Projects one listed item into its row (T2 §5).
fn project(item: PartialObjectMetadata) -> SearchRow {
    let (group, version) = match item.api_version.split_once('/') {
        Some((group, version)) => (group.to_string(), Some(version.to_string())),
        None => (item.api_version, None),
    };

    SearchRow {
        name: item.metadata.name,
        kind: item.kind,
        title: item.metadata.title,
        group,
        version,
        family: item.metadata.family,
        labels: item.metadata.labels,
    }
}

/// The filters as the tool interpreted them, for an empty result (T2 §6).
fn interpreted_filters(input: &SearchCatalogInput) -> Value {
    let mut filters = Map::new();

    if let Some(query) = &input.query {
        filters.insert("query".to_string(), json!(query));
    }
    if let Some(kind) = &input.kind {
        filters.insert("kind".to_string(), json!(kind));
    }
    if let Some(labels) = input.labels.as_ref().filter(|labels| !labels.is_empty()) {
        filters.insert("labels".to_string(), json!(labels));
    }
    if let Some(fields) = input.fields.as_ref().filter(|fields| !fields.is_empty()) {
        filters.insert("fields".to_string(), json!(fields));
    }

    Value::Object(filters)
}

#[cfg(test)]
mod tests;
