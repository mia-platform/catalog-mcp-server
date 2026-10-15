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
// `get_audit_log`: who did what in the catalog, and when. The audit log is the one listing the
// engine pages by offset, so the cursor pins a window and the rows already returned; an empty
// answer says what the log does not record rather than that nothing happened.

use crate::{
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::{
        apply_item::{MAX_KIND_BYTES, MAX_NAME_BYTES},
        arguments::validate_group,
    },
};
use catalog_client::{
    EngineClient, Remedy, ToolError, coordinates_of,
    error::{BadRequestOrigin, codes},
    find_item_type_or_suggest, is_valid_kind, is_valid_name,
    models::AuditLogEntry,
    ops::audit_log::AuditLogQuery,
    pagination::{MAX_LIMIT, ToolCursor, fingerprint},
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use configuration::tools::ToolDefaults;
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The pinned window and its paging rules.
mod window;

/// An entry as the model reads it.
mod shape;

use shape::ShapedEntry;
use window::{AuditWindow, MAX_SKIP, Next};

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "get_audit_log";

/// What the tool does. Names every filter and the two things a model gets wrong without being
/// told: an empty answer is not proof, and the names are today's.
const TOOL_DESCRIPTION: &str = "Who did what in the catalog, and when: audit-log `entries`, \
     newest first, with `total`. Filters: `since`/`until` (a date, read as midnight UTC, or a \
     date-time; `until` exclusive), `principal` (entries' `whoId`), `action` codes (e.g. \
     `item.upsert`), `outcome`, `scope` (`instance`: one resource; `collection`: a whole type; \
     `none`: tenant-wide), `name` with `kind` for one item, `kind` alone for a type. Names are \
     current, even on old entries. Writes are recorded only while auditing is on, most reads \
     never: an empty result does not mean nothing happened. Pass `cursor` back with the same \
     filters; rows already returned are skipped, so a page can be shorter. Needs audit-log read \
     permission.";

/// The most principals or action codes one call may filter on — input hygiene: the engine sets
/// no bound, the query string does.
pub const MAX_FILTER_VALUES: usize = 20;

/// The prefix of the audit log's own read actions, which the engine leaves out of every listing
/// unless asked to include them — and this tool never asks.
const AUDIT_LOG_ACTION_PREFIX: &str = "auditLog.";

/// The type definition URN's fixed part: the group, version and kind the engine files every type
/// definition under, followed by its `metadata.name`.
const TYPE_DEFINITION_URN_PREFIX: &str =
    "urn:mia-platform-catalog:mia-platform.eu:v1:ItemTypeDefinition:";

/// The item URN's scheme.
const ITEM_URN_PREFIX: &str = "urn:mia-platform-catalog:";

/// The date format the engine reads as midnight UTC.
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Said with an empty answer: what the log records, and how to find out whether it was off.
const EMPTY_HINT: &str = "No entry matches. Writes are recorded while auditing is on — those \
     refused by authorization never are; reads only for item types that opt in, apart from a few \
     administrative reads; some actions are never recorded. So this does not prove nothing \
     happened. To see whether auditing was switched off in this period, filter `action: \
     tenantSettings.update`.";

/// Said when the walk reaches the furthest page the engine serves.
const PAST_CAP_WARNING: &str = "More entries match, beyond the 10 000 the audit log can page \
     through: narrow `since`/`until` or the other filters to see them.";

/// The message of a `403` from the audit log.
const FORBIDDEN_MESSAGE: &str = "Reading the audit log needs the permission to read it, which \
     the admin and auditor roles grant; this caller does not have it.";

/// The next step for a `kind` that does not resolve: a deleted type's own history is still there.
const DELETED_TYPE_NEXT_STEP: &str = "if the type was deleted, call get_audit_log with `action: \
     [\"itd.delete\"]` and no `kind`";

/// The next step for `name` without `kind`.
const NAME_WITHOUT_KIND_NEXT_STEP: &str = "find the item's kind with describe_item or \
     search_catalog, then call again with `name` and `kind`";

/// How a call ended.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    // Succeeded.
    Success,
    // Was refused or failed.
    Error,
}

/// What an entry names.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    // One resource.
    Instance,
    // A whole type.
    Collection,
    // Nothing: a tenant-wide action.
    None,
}

impl Outcome {
    /// The engine's value.
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

impl Scope {
    /// The engine's value.
    fn as_str(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Collection => "collection",
            Self::None => "none",
        }
    }
}

/// Arguments for `get_audit_log`. Every one is a filter, and all are optional.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[serde(deny_unknown_fields)]
pub struct GetAuditLogInput {
    /// With `kind`: one item.
    #[serde(rename = "name")]
    pub name: Option<String>,

    /// The type; alone, the whole type.
    #[serde(rename = "kind")]
    pub kind: Option<String>,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// Inclusive.
    #[serde(rename = "since")]
    pub since: Option<String>,

    /// Exclusive.
    #[serde(rename = "until")]
    pub until: Option<String>,

    /// Entries' `whoId` values.
    #[serde(rename = "principal")]
    pub principal: Option<Vec<String>>,

    /// Action codes.
    #[serde(rename = "action")]
    pub action: Option<Vec<String>>,

    #[serde(rename = "outcome")]
    pub outcome: Option<Outcome>,

    #[serde(rename = "scope")]
    pub scope: Option<Scope>,

    /// Default 50, at most 200.
    #[serde(rename = "limit")]
    pub limit: Option<u32>,

    /// From the previous page.
    #[serde(rename = "cursor")]
    pub cursor: Option<String>,
}

/// The answer: a page of shaped entries and how to get the next one.
#[derive(Serialize)]
struct AuditAnswer {
    #[serde(rename = "entries")]
    entries: Vec<ShapedEntry>,

    #[serde(rename = "total")]
    total: u64,

    #[serde(rename = "totalIsExact")]
    total_is_exact: bool,

    /// The page size used, **only** when the caller's was clamped.
    #[serde(rename = "limit", skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,

    #[serde(rename = "cursor", skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,

    /// The query as sent, **only** on an empty first page, so "nothing recorded" can be told from
    /// "filtered wrongly".
    #[serde(rename = "filters", skip_serializing_if = "Option::is_none")]
    filters: Option<Value>,

    /// What the log does not record, **only** on an empty first page.
    #[serde(rename = "hint", skip_serializing_if = "Option::is_none")]
    hint: Option<&'static str>,
}

/// Where the filters point, resolved from `name`, `kind` and `group`.
#[derive(Default)]
struct Target {
    /// One item's URN.
    resource_urn: Option<String>,

    /// A type definition's URN.
    resource_type_urn: Option<String>,

    /// Said when an item's type serves several versions.
    warning: Option<String>,
}

/// `get_audit_log` — read the tenant's audit log.
pub struct GetAuditLog {
    default_limit: u32,
}

impl GetAuditLog {
    /// The tool, with the page size the configuration sets.
    pub fn new(defaults: &ToolDefaults) -> Self {
        Self {
            default_limit: defaults.audit_limit.clamp(1, MAX_LIMIT),
        }
    }
}

impl Tool for GetAuditLog {
    type Input = GetAuditLogInput;

    /// Read-only.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<GetAuditLogInput>(
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
        let caller_until = validate(&input)?;
        let engine = context.engine();

        let limit = input
            .limit
            .map_or(self.default_limit, |limit| limit.clamp(1, MAX_LIMIT));
        let clamped = input.limit.filter(|asked| *asked != limit).map(|_| limit);

        let target = resolve_target(engine, &input).await?;

        let mut query = AuditLogQuery {
            since: input.since.clone(),
            until: input.until.clone(),
            principals: input.principal.clone().unwrap_or_default(),
            actions: input.action.clone().unwrap_or_default(),
            outcome: input.outcome.map(|outcome| outcome.as_str().to_string()),
            scope: input.scope.map(|scope| scope.as_str().to_string()),
            resource_urn: target.resource_urn.clone(),
            resource_type_urn: target.resource_type_urn.clone(),
            skip: 0,
            limit,
        };
        let query_fingerprint = fingerprint(&filters_json(&query));

        // A later page re-sends filters the engine already accepted, under a window of ours: a
        // `400` there is this server's, never a loop the model cannot win.
        let (window, origin) = match &input.cursor {
            Some(raw) => {
                let window = ToolCursor::<AuditWindow>::decode(raw, &query_fingerprint)?.pinned;
                if window.skip > MAX_SKIP {
                    return Err(unusable_cursor());
                }
                query.skip = window.skip;
                query.until = Some(window.until.clone());
                (Some(window), BadRequestOrigin::ServerBuilt)
            }
            None => (None, BadRequestOrigin::CallerInput),
        };

        let page = match engine.list_audit_log(&query, origin).await {
            Ok(response) => response.value,
            Err(error) => return Err(refused(engine, &query, error).await),
        };

        let fetched = page.items.len() as u64;
        let newest = page.items.first().map(|entry| entry.timestamp.clone());
        let items: Vec<AuditLogEntry> = match &window {
            Some(window) => window::drop_seen(page.items, &window.last),
            None => page.items,
        };

        let next = window::next_page(
            query.skip,
            fetched,
            u64::from(limit),
            page.metadata.total,
            page.metadata.total_is_exact,
        );

        let cursor = match next {
            Next::More(skip) => {
                let until = match &window {
                    Some(window) => Some(window.until.clone()),
                    None => newest
                        .as_deref()
                        .and_then(|newest| window::pin_until(newest, caller_until))
                        .map(window::format_instant),
                };
                let last = items
                    .last()
                    .map(|entry| entry.uid.clone())
                    .or_else(|| window.as_ref().map(|window| window.last.clone()));

                match (until, last) {
                    (Some(until), Some(last)) => Some(
                        ToolCursor::new(
                            None,
                            &query_fingerprint,
                            AuditWindow { skip, until, last },
                        )
                        .encode()?,
                    ),
                    _ => None,
                }
            }
            Next::End | Next::PastCap => None,
        };

        let empty_first_page = items.is_empty() && window.is_none();
        let answer = AuditAnswer {
            entries: items.into_iter().map(shape::shape).collect(),
            total: page.metadata.total,
            total_is_exact: page.metadata.total_is_exact,
            limit: clamped,
            cursor,
            filters: empty_first_page.then(|| filters_json(&query)),
            hint: empty_first_page.then_some(EMPTY_HINT),
        };

        let mut output = serde_json::to_value(answer)
            .map(ToolOutput::new)
            .map_err(|err| {
                ToolError::new(
                    codes::SERVER_DEFECT,
                    Remedy::Escalate,
                    format!("The audit log page could not be rendered: {err}"),
                )
            })?;

        if next == Next::PastCap {
            output = output.with_warning(PAST_CAP_WARNING);
        }
        if let Some(warning) = target.warning {
            output = output.with_warning(warning);
        }

        Ok(output)
    }
}

/// The URN the filters point at: one item's, built without reading it so a deleted item's history
/// is still found; or a whole type's, the type definition's own URN as the engine files it.
async fn resolve_target(
    engine: &EngineClient,
    input: &GetAuditLogInput,
) -> Result<Target, ToolError> {
    let Some(kind) = input.kind.as_deref() else {
        return Ok(Target::default());
    };

    let definition = find_item_type_or_suggest(engine, kind, input.group.as_deref())
        .await
        .map_err(|error| {
            if error.code == codes::NOT_FOUND {
                error.with_next_step(DELETED_TYPE_NEXT_STEP)
            } else {
                error
            }
        })?;

    let Some(name) = input.name.as_deref() else {
        return Ok(Target {
            resource_type_urn: Some(format!(
                "{TYPE_DEFINITION_URN_PREFIX}{}",
                definition.metadata.name
            )),
            ..Target::default()
        });
    };

    // The engine files an item's entries under the version of the request that touched it.
    let coordinates = coordinates_of(&definition, kind)?;
    let served = definition
        .spec
        .versions
        .iter()
        .filter(|version| version.served)
        .count();

    Ok(Target {
        resource_urn: Some(format!(
            "{ITEM_URN_PREFIX}{}:{}:{}:{name}",
            coordinates.group, coordinates.version, definition.spec.names.kind
        )),
        warning: (served > 1).then(|| {
            format!(
                "`{}` serves several versions; this is the history recorded through `{}`. Entries \
                 made through the other versions are not included.",
                definition.spec.names.kind, coordinates.version
            )
        }),
        ..Target::default()
    })
}

/// What a refused page tells the model.
///
/// A `403` is about the audit log's own permission; a `400` on the caller's first page is
/// explained — an unknown action code with the codes that exist, anything else with the tool's own
/// parameter names. A `400` on a later page is already this server's, as the client reported it.
async fn refused(engine: &EngineClient, query: &AuditLogQuery, error: ToolError) -> ToolError {
    match error.code {
        codes::FORBIDDEN => ToolError {
            message: FORBIDDEN_MESSAGE.to_string(),
            ..error
        },
        codes::INVALID_INPUT if !query.actions.is_empty() => {
            unknown_actions(engine, &query.actions, error).await
        }
        codes::INVALID_INPUT => with_tool_names(error),
        _ => error,
    }
}

/// The engine's `400` while action codes were given: when some code is not in the vocabulary,
/// the answer names it and lists the codes that exist; otherwise the refusal was about something
/// else and is relayed. The engine's own list is never parsed out of its message.
async fn unknown_actions(engine: &EngineClient, given: &[String], error: ToolError) -> ToolError {
    let vocabulary = match engine.list_audit_log_actions().await {
        Ok(response) => response.value,
        Err(listing_error) => {
            tracing::warn!(error = %listing_error, "the audit-log actions could not be listed");
            return with_tool_names(error);
        }
    };

    let unknown: Vec<&str> = given
        .iter()
        .filter(|code| !vocabulary.iter().any(|action| &action.code == *code))
        .map(String::as_str)
        .collect();

    if unknown.is_empty() {
        return with_tool_names(error);
    }

    let valid: Vec<Value> = vocabulary
        .iter()
        .filter(|action| !action.code.starts_with(AUDIT_LOG_ACTION_PREFIX))
        .map(|action| json!({ "code": action.code, "displayName": action.display_name }))
        .collect();

    let mut details = existing_details(&error);
    details.insert("field".to_string(), json!("action"));
    details.insert("validActions".to_string(), Value::Array(valid));

    ToolError::new(
        codes::INVALID_INPUT,
        Remedy::RetryAfterChange,
        format!(
            "{} not an action code; `validActions` lists the codes that exist.",
            unknown
                .iter()
                .map(|code| format!("`{code}`"))
                .collect::<Vec<_>>()
                .join(", ")
                + if unknown.len() == 1 { " is" } else { " are" }
        ),
    )
    .with_details(Value::Object(details))
}

/// The engine's message, with this tool's names for the two parameters it renames.
fn with_tool_names(error: ToolError) -> ToolError {
    let message = error
        .message
        .replace("'occurredSince'", "'occurredSince' (`since`)")
        .replace("'occurredUntil'", "'occurredUntil' (`until`)");

    ToolError { message, ..error }
}

/// The error's own details — its `requestId`, say — to add to.
fn existing_details(error: &ToolError) -> Map<String, Value> {
    error
        .details
        .as_deref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// A cursor whose offset the engine would refuse — not one this server minted.
fn unusable_cursor() -> ToolError {
    ToolError::new(
        codes::INVALID_CURSOR,
        Remedy::RetryAfterChange,
        "The cursor is not one this tool returned.",
    )
    .with_next_step("call again with the same filters and no `cursor`")
}

/// The query's filters as sent, for the fingerprint and for an empty answer's echo.
fn filters_json(query: &AuditLogQuery) -> Value {
    let mut filters = Map::new();

    for (name, value) in [
        ("since", &query.since),
        ("until", &query.until),
        ("outcome", &query.outcome),
        ("scope", &query.scope),
        ("resourceUrn", &query.resource_urn),
        ("resourceTypeUrn", &query.resource_type_urn),
    ] {
        if let Some(value) = value {
            filters.insert(name.to_string(), json!(value));
        }
    }
    if !query.principals.is_empty() {
        filters.insert("principal".to_string(), json!(query.principals));
    }
    if !query.actions.is_empty() {
        filters.insert("action".to_string(), json!(query.actions));
    }
    filters.insert("limit".to_string(), json!(query.limit));

    Value::Object(filters)
}

/// The input bounds, checked before anything reaches the engine. Returns the caller's `until` as
/// an instant, which the window pins against.
fn validate(input: &GetAuditLogInput) -> Result<Option<DateTime<Utc>>, ToolError> {
    if let Some(name) = &input.name {
        if input.kind.is_none() {
            return Err(invalid(
                "name",
                "`name` needs `kind`: an item is found by its type and name.".to_string(),
            )
            .with_next_step(NAME_WITHOUT_KIND_NEXT_STEP));
        }
        if name.is_empty() || name.len() > MAX_NAME_BYTES || !is_valid_name(name) {
            return Err(invalid(
                "name",
                format!(
                    "`{name}` is not an item name: lowercase letters, digits, `-` and `.`, \
                     starting and ending with a letter or digit."
                ),
            ));
        }
    }

    if let Some(kind) = &input.kind
        && (kind.is_empty() || kind.len() > MAX_KIND_BYTES || !is_valid_kind(kind))
    {
        return Err(invalid(
            "kind",
            format!("`{kind}` is not a kind: a kind is a letter followed by letters and digits."),
        )
        .with_next_step("call list_catalog_types to see the kinds that exist"));
    }

    validate_group(input.group.as_deref(), input.kind.is_some())?;

    let since = input
        .since
        .as_deref()
        .map(|since| parse_instant("since", since))
        .transpose()?;
    let until = input
        .until
        .as_deref()
        .map(|until| parse_instant("until", until))
        .transpose()?;
    if let (Some(since), Some(until)) = (since, until)
        && since >= until
    {
        return Err(invalid(
            "since",
            "`since` must be before `until`, which is exclusive.".to_string(),
        ));
    }

    if let Some(principals) = &input.principal {
        bounded("principal", principals.len())?;
        if let Some(bad) = principals
            .iter()
            .find(|principal| uuid::Uuid::parse_str(principal).is_err())
        {
            return Err(invalid(
                "principal",
                format!("`{bad}` is not a principal id: give the `whoId` of an entry."),
            ));
        }
    }

    if let Some(actions) = &input.action {
        bounded("action", actions.len())?;
        if let Some(own) = actions
            .iter()
            .find(|action| action.starts_with(AUDIT_LOG_ACTION_PREFIX))
        {
            return Err(invalid(
                "action",
                format!("`{own}` records reads of the audit log itself, which are not shown."),
            ));
        }
    }

    Ok(until)
}

/// An instant as the engine reads it: an RFC 3339 date-time, or a bare date as midnight UTC.
fn parse_instant(field: &str, text: &str) -> Result<DateTime<Utc>, ToolError> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(text) {
        return Ok(instant.with_timezone(&Utc));
    }

    NaiveDate::parse_from_str(text, DATE_FORMAT)
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|midnight| Utc.from_utc_datetime(&midnight))
        .ok_or_else(|| {
            invalid(
                field,
                format!(
                    "`{text}` is not a date (`2026-09-17`) or a date-time \
                     (`2026-09-17T10:30:45Z`)."
                ),
            )
        })
}

/// Refuses more than [`MAX_FILTER_VALUES`] values for one filter.
fn bounded(field: &str, count: usize) -> Result<(), ToolError> {
    if count > MAX_FILTER_VALUES {
        return Err(invalid(
            field,
            format!("`{field}` takes at most {MAX_FILTER_VALUES} values; {count} were given."),
        ));
    }

    Ok(())
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

#[cfg(test)]
mod tests;
