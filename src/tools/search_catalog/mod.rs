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
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::filters::{
        self as ast, FieldFilters, Filters, SPEC_WITHOUT_KIND_WARNING, filters_on_spec,
        is_relationships, resolve_family,
    },
};
use catalog_client::{
    Predicate, Remedy, ToolError,
    error::codes,
    models::{Item, ObjectMetadata, PartialObjectMetadata},
    ops::ListQuery,
    pagination::{DEFAULT_LIMIT, ListPage, MAX_LIMIT},
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The scheme every catalog URN starts with, `urn:mia-platform-catalog:<group>:<version>:<kind>:<name>`.
const URN_PREFIX: &str = "urn:mia-platform-catalog:";

/// What a search cursor pins, and its fingerprint.
mod cursor;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "search_catalog";

/// What the tool does. No `rawq`, no base64, no coordinates, no pagination mechanics.
///
/// The `Relationship` sentence is there because output fields are learned only from answers: an
/// agent that does not know relationship rows name their ends would `describe_item` each one.
const TOOL_DESCRIPTION: &str = "Searches the catalog. Use `query` for free text over names, \
     titles and tags; `kind` to restrict to one type; `labels` and `fields` to filter exactly. \
     `Relationship` rows include their `type`, `source` and `target`. Call `list_catalog_types` \
     if unsure of the `kind`. Add `group` for a shared `kind`. `total` counts every match; pass \
     `cursor` back for more; `limit` appears only if yours was clamped; an empty result echoes \
     the `filters` as understood. Rows carry `owner` and `recordUpdatedAt` if set.";

/// The `spec` paths that hold a relationship's ends, each an item's `urn`.
const RELATIONSHIP_END_FIELDS: [&str; 2] = ["spec.sourceRef", "spec.targetRef"];

/// What an empty `Relationship` search by free text is answered with.
///
/// Free text matches a relationship's own name, title and tags, never the items it connects, so
/// searching an item's name among relationships finds nothing — and an agent that sees nothing
/// concludes the item has no relationships. The ends are filterable, by the item's `urn`.
const RELATIONSHIP_QUERY_HINT: &str = "`query` matches a relationship's name, title and tags, not \
     its ends. To find an item's relationships, filter `fields` on `spec.sourceRef` or \
     `spec.targetRef` with the item's `urn` from describe_item, or read them with describe_item.";

/// The smallest page. The engine checks only the upper bound, so a `0` would be sent as is.
const MIN_LIMIT: u32 = 1;

/// The bounds on the filters, which `search_catalog` shares with every tool taking its filters,
/// at the path its tests name them by.
#[cfg(test)]
pub(crate) use crate::tools::filters::{MAX_FILTER_ENTRIES, MAX_QUERY_BYTES};

/// Arguments for `search_catalog`.
///
/// `BTreeMap` for both maps, so the query — and therefore the cursor fingerprint — does not
/// depend on the order the model emitted its keys in. No `#[serde(default)]`: an absent `Option`
/// is already `None`, and the attribute would put `"default": null` into the schema.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct SearchCatalogInput {
    /// Free text over names, titles and tags.
    #[serde(rename = "query")]
    pub query: Option<String>,

    /// Restrict to one type, e.g. "Service".
    #[serde(rename = "kind")]
    pub kind: Option<String>,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// Exact-match label filters.
    #[serde(rename = "labels")]
    pub labels: Option<BTreeMap<String, String>>,

    /// Exact-match field filters, by JSON path; null matches an unset field.
    #[serde(rename = "fields")]
    pub fields: Option<FieldFilters>,

    /// Page size. Default 50, clamped to 200.
    #[serde(rename = "limit")]
    pub limit: Option<u16>,

    /// The cursor a previous call returned, to fetch the next page.
    //
    // Not worded "opaque continuation token": the schema guard refuses the word "token" in any
    // schema, because that is how an identity parameter would reappear.
    #[serde(rename = "cursor")]
    pub cursor: Option<String>,
}

/// One result row, with everything needed to act on it without a second lookup. Every field
/// comes from the item's metadata, except a relationship's `type`, `source` and `target`, which come
/// from its `spec`.
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

    /// `metadata.family`. Absent only for an item whose type no longer exists.
    #[serde(rename = "family", skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,

    /// `metadata.labels`, omitted when there are none.
    #[serde(rename = "labels", skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,

    /// `metadata.owner`, the catalog record's own owner: an identity reference (`{type, ref}`)
    /// passed through as stored, exactly as `describe_item` shows it. Omitted when the record has
    /// none, so a row without it is an item without an owner — which is what makes ownership
    /// questions answerable from a search alone.
    #[serde(rename = "owner", skip_serializing_if = "Option::is_none")]
    pub owner: Option<Value>,

    /// `metadata.updateTimestamp`: when the catalog record last changed, under the same name as in
    /// `describe_item`. It is not a source system's own date inside `spec`.
    #[serde(rename = "recordUpdatedAt", skip_serializing_if = "Option::is_none")]
    pub record_updated_at: Option<String>,

    /// A relationship's type, the last segment of `spec.typeRef`. Only on relationship rows.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub relationship_type: Option<String>,

    /// The item a relationship starts from, from `spec.sourceRef`. Only on relationship rows.
    #[serde(rename = "source", skip_serializing_if = "Option::is_none")]
    pub source: Option<RelationshipEnd>,

    /// The item a relationship points at, from `spec.targetRef`. Only on relationship rows.
    #[serde(rename = "target", skip_serializing_if = "Option::is_none")]
    pub target: Option<RelationshipEnd>,
}

/// One end of a relationship, addressed as `describe_item` takes it.
///
/// A relationship's `spec` is three references and nothing else, so without them a relationship
/// row says nothing about what it connects, and every row would need a `describe_item` of its
/// own. The ends are parsed from their URNs; whether each item still exists is not checked — the
/// catalog accepts a relationship to an item that does not exist — so a dangling end looks like
/// any other. A reference that is not a catalog URN is passed on whole, as `urn`.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct RelationshipEnd {
    #[serde(rename = "name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    #[serde(rename = "group", skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(rename = "urn", skip_serializing_if = "Option::is_none")]
    pub urn: Option<String>,
}

/// The whole response.
#[derive(Serialize)]
struct SearchOutput {
    #[serde(rename = "items")]
    items: Vec<SearchRow>,

    /// Every match, counted only when the page was full.
    #[serde(rename = "total")]
    total: u64,

    /// Absent when there are no more results.
    #[serde(rename = "cursor", skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,

    /// The effective page size, **only** when it differs from the one asked for: a
    /// silent clamp would let the model believe it had seen everything.
    #[serde(rename = "limit", skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,

    /// The filters as interpreted, **only** on an empty result, so *"nothing matched"* can be told
    /// from *"I filtered wrongly"*.
    #[serde(rename = "filters", skip_serializing_if = "Option::is_none")]
    filters: Option<Value>,

    /// The way to what the search most likely wanted, **only** on an empty result whose shape
    /// shows a known dead end.
    #[serde(rename = "hint", skip_serializing_if = "Option::is_none")]
    hint: Option<&'static str>,
}

/// `search_catalog` — find items by free text, type, labels and fields.
///
/// One listing — global, or scoped to a family when `kind` is given — plus a count only when the
/// page is full. The query translator is `catalog-client`'s; this tool builds the AST and chooses
/// the endpoint.
pub struct SearchCatalog;

impl Tool for SearchCatalog {
    type Input = SearchCatalogInput;

    /// `readOnlyHint: true`; every other hint is the specification's default.
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
        let fingerprint = cursor::fingerprint(
            input.kind.as_deref(),
            input.group.as_deref(),
            predicate.as_ref(),
        );
        let engine = context.engine();

        let (family, engine_cursor, returned) = match &input.cursor {
            Some(raw) => {
                let resumed = cursor::resume(raw, &fingerprint, input.kind.is_some())?;
                (resumed.family, Some(resumed.engine), resumed.returned)
            }
            None => {
                let family = match &input.kind {
                    Some(kind) => Some(
                        resolve_family(engine, kind, input.group.as_deref(), input.fields.as_ref())
                            .await?,
                    ),
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

        // Relationships are read whole, `spec` included, because their `spec` is what they mean;
        // every other search reads the metadata-only projection.
        let page: ListPage<SearchRow> = match &family {
            Some(family) if is_relationships(family) => into_rows(
                engine.list_family_items(family, &query).await,
                project_relationship,
            ),
            Some(family) => into_rows(
                engine.list_family_items_partial(family, &query).await,
                project,
            ),
            None => into_rows(engine.list_items_partial(&query).await, project),
        }
        .inspect_err(|error| log_if_ours(error, predicate.as_ref()))?;

        // A page that is not full already says how many there are.
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

        // The inputs to any future tuning. `rawq_parameters` should be 1 in every real
        // search; more means the split rule fired.
        tracing::debug!(count_fired, rawq_parameters, "searched the catalog");

        let returned_now = returned + page.items.len() as u64;
        let next_cursor = page
            .next
            .as_ref()
            .map(|next| cursor::mint(next, &fingerprint, family.as_ref(), returned_now))
            .transpose()?;

        // The page is emitted whole: the engine's cursor points after what was fetched.
        let items = page.items;
        let filters = items.is_empty().then(|| interpreted_filters(&input));
        let hint = (items.is_empty()
            && family.as_ref().is_some_and(is_relationships)
            && input.query.is_some()
            && !filters_on_an_end(input.fields.as_ref()))
        .then_some(RELATIONSHIP_QUERY_HINT);

        let output = SearchOutput {
            items,
            total,
            cursor: next_cursor,
            limit: limit_echo,
            filters,
            hint,
        };

        let payload = serde_json::to_value(&output).map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The search result could not be rendered: {err}"),
            )
        })?;

        let output = ToolOutput::new(payload);

        Ok(
            if input.kind.is_none() && filters_on_spec(input.fields.as_ref()) {
                output.with_warning(SPEC_WITHOUT_KIND_WARNING)
            } else {
                output
            },
        )
    }
}

/// Whether `fields` filters a relationship by one of its ends.
fn filters_on_an_end(fields: Option<&FieldFilters>) -> bool {
    fields
        .into_iter()
        .flat_map(BTreeMap::keys)
        .any(|path| RELATIONSHIP_END_FIELDS.contains(&path.as_str()))
}

/// Every input bound, checked before anything reaches the engine.
fn validate(input: &SearchCatalogInput) -> Result<(), ToolError> {
    ast::validate(&Filters {
        query: input.query.as_deref(),
        kind: input.kind.as_deref(),
        group: input.group.as_deref(),
        labels: input.labels.as_ref(),
        fields: input.fields.as_ref(),
    })
}

/// The page size to ask for, and what to echo.
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

/// A `400` on a query this server built is logged with the **decoded** query, never the
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

/// A listed page as rows.
fn into_rows<T>(
    page: Result<catalog_client::EngineResponse<ListPage<T>>, ToolError>,
    project: fn(T) -> SearchRow,
) -> Result<ListPage<SearchRow>, ToolError> {
    page.map(|response| ListPage {
        items: response.value.items.into_iter().map(project).collect(),
        next: response.value.next,
    })
}

/// Projects one listed item into its row.
fn project(item: PartialObjectMetadata) -> SearchRow {
    row(item.api_version, item.kind, item.metadata)
}

/// Projects one relationship into its row: the item's own fields, and what it connects.
fn project_relationship(item: Item) -> SearchRow {
    let reference = |field: &str| {
        item.spec
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let (source, target, type_ref) = (
        reference("sourceRef"),
        reference("targetRef"),
        reference("typeRef"),
    );

    SearchRow {
        relationship_type: type_ref
            .as_deref()
            .and_then(|urn| urn.rsplit(':').next())
            .map(str::to_string),
        source: source.map(|urn| relationship_end(&urn)),
        target: target.map(|urn| relationship_end(&urn)),
        ..row(item.api_version, item.kind, item.metadata)
    }
}

/// The fields every row carries.
fn row(api_version: String, kind: String, metadata: ObjectMetadata) -> SearchRow {
    let (group, version) = match api_version.split_once('/') {
        Some((group, version)) => (group.to_string(), Some(version.to_string())),
        None => (api_version, None),
    };

    SearchRow {
        name: metadata.name,
        kind,
        title: metadata.title,
        group,
        version,
        family: metadata.family,
        labels: metadata.labels,
        owner: metadata.owner,
        record_updated_at: metadata.update_timestamp,
        relationship_type: None,
        source: None,
        target: None,
    }
}

/// One end of a relationship, from its URN `urn:mia-platform-catalog:<group>:<version>:<kind>:<name>`.
fn relationship_end(urn: &str) -> RelationshipEnd {
    let parts: Option<[&str; 4]> = urn
        .strip_prefix(URN_PREFIX)
        .map(|rest| rest.split(':').collect::<Vec<_>>())
        .and_then(|parts| parts.try_into().ok());

    match parts {
        Some([group, _version, kind, name])
            if !group.is_empty() && !kind.is_empty() && !name.is_empty() =>
        {
            RelationshipEnd {
                name: Some(name.to_string()),
                kind: Some(kind.to_string()),
                group: Some(group.to_string()),
                urn: None,
            }
        }
        _ => RelationshipEnd {
            name: None,
            kind: None,
            group: None,
            urn: Some(urn.to_string()),
        },
    }
}

/// The filters as the tool interpreted them, for an empty result.
fn interpreted_filters(input: &SearchCatalogInput) -> Value {
    let mut filters = Map::new();

    if let Some(query) = &input.query {
        filters.insert("query".to_string(), json!(query));
    }
    if let Some(kind) = &input.kind {
        filters.insert("kind".to_string(), json!(kind));
    }
    if let Some(group) = &input.group {
        filters.insert("group".to_string(), json!(group));
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
