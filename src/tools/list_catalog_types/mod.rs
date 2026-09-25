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
    Remedy, ToolError,
    error::{cancelled, codes},
    models::ItdListEntry,
    ops::ListQuery,
    pagination::{MAX_LIMIT, paginate_all},
    select_served_version,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "list_catalog_types";

/// What the tool does (T1 §3.3, adopted from the analysis unchanged).
///
/// Note what it omits: nothing about `group`/`version`/`family` being needed elsewhere, no
/// pagination instructions. If the model had to be told any of that, the tool set would have
/// failed.
const TOOL_DESCRIPTION: &str = "Lists every item type in the catalog, with the coordinates needed \
     to address items of that type. Call this first when you do not already know a type's exact \
     `kind`. Returns every type in one response — there is no pagination. Use `search` to \
     narrow by name or purpose.";

/// The longest `search` term accepted, in bytes (T1 §3.1, NFR-10).
///
/// Nothing legitimate needs more, and it keeps the filter's cost bounded.
pub const MAX_SEARCH_BYTES: usize = 200;

/// The input field `search`, as named in errors.
const SEARCH_FIELD: &str = "search";

/// Arguments for `list_catalog_types`.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct ListCatalogTypesInput {
    /// Case-insensitive substring, matched over kind, family, display name and description.
    //
    // No `#[serde(default)]`: an absent `Option` is already `None`, and the attribute would put
    // `"default": null` into the schema every `tools/list` pays for.
    #[serde(rename = "search")]
    pub search: Option<String>,
}

/// One row of the response. Field order here is the serialised order (T1 §4).
///
/// Absent fields are **omitted, never null**: a null costs bytes and tells the model nothing.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub struct CatalogType {
    /// `spec.names.kind` — what every other tool takes as `kind`.
    #[serde(rename = "kind")]
    pub kind: String,

    /// `spec.names.plural`, the path segment items of this type live under.
    #[serde(rename = "family")]
    pub family: String,

    /// `spec.group`.
    #[serde(rename = "group")]
    pub group: String,

    /// The served version chosen by the core's rule (§8.6, T1-D4).
    #[serde(rename = "version")]
    pub version: String,

    /// `spec.names.displayPlural`, when the type declares one.
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// `spec.llmDescription`, **verbatim** (T1 §7) — omitted when absent or blank, and never
    /// synthesised from anything else.
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// `spec.history.enabled`, defaulting to `false`.
    #[serde(rename = "historyEnabled")]
    pub history_enabled: bool,
}

/// The whole response (T1 §4).
///
/// `search` and `filteredFrom` are present **whenever a search was supplied**, so an empty
/// result can always be told apart: *"nothing matched your term"* is not *"you have no types"*
/// (T1-D9), and echoing the term back is what lets the model notice its own typo.
#[derive(Serialize)]
struct ListCatalogTypesOutput {
    #[serde(rename = "types")]
    types: Vec<CatalogType>,

    #[serde(rename = "total")]
    total: usize,

    #[serde(rename = "search", skip_serializing_if = "Option::is_none")]
    search: Option<String>,

    #[serde(rename = "filteredFrom", skip_serializing_if = "Option::is_none")]
    filtered_from: Option<usize>,
}

/// T1 · `list_catalog_types` — every item type the caller can see, with its coordinates.
///
/// One engine listing, walked to exhaustion, projected, optionally filtered, sorted. Nothing is
/// cached (D31, T1-D1): every call fetches the caller's own catalogue, which is what makes it
/// impossible for one tenant to be answered with another's.
pub struct ListCatalogTypes;

impl Tool for ListCatalogTypes {
    type Input = ListCatalogTypesInput;

    /// `readOnlyHint: true`; every other hint is the specification's default (D16).
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<ListCatalogTypesInput>(
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
        // NFR-10 — validated at the boundary, before anything reaches the engine.
        if let Some(search) = &input.search {
            validate_search(search)?;
        }

        // Rule 5 — this tool loops over pages, so it stops when the caller goes away.
        let entries = tokio::select! {
            biased;
            () = context.cancellation().cancelled() => return Err(cancelled()),
            entries = fetch_every_type(context) => entries?,
        };

        let fetched = entries.len();
        let mut types: Vec<CatalogType> = entries.into_iter().filter_map(project).collect();

        // T1 §10 — the inputs to T1-D2's future decision. `omitted` is zero everywhere today; a
        // non-zero value is the first sign the version-selection rule has started firing.
        tracing::debug!(
            fetched,
            omitted = fetched - types.len(),
            "listed the catalog's item types"
        );

        // T1-D8 — by `kind`, byte-wise; `group` then `family` break the tie two groups sharing
        // a kind would otherwise leave to the engine's unspecified order.
        types.sort_by(|left, right| {
            (&left.kind, &left.group, &left.family).cmp(&(&right.kind, &right.group, &right.family))
        });

        let output = match input.search {
            None => ListCatalogTypesOutput {
                total: types.len(),
                types,
                search: None,
                filtered_from: None,
            },
            Some(search) => {
                let filtered_from = types.len();
                let needle = search.to_lowercase();
                types.retain(|row| matches_search(row, &needle));

                ListCatalogTypesOutput {
                    total: types.len(),
                    types,
                    search: Some(search),
                    filtered_from: Some(filtered_from),
                }
            }
        };

        let payload = serde_json::to_value(&output).map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The type listing could not be rendered: {err}"),
            )
        })?;

        Ok(ToolOutput::new(payload))
    }
}

/// Rejects a `search` term longer than [`MAX_SEARCH_BYTES`].
fn validate_search(search: &str) -> Result<(), ToolError> {
    if search.len() <= MAX_SEARCH_BYTES {
        return Ok(());
    }

    Err(ToolError::new(
        codes::INVALID_INPUT,
        Remedy::RetryAfterChange,
        format!(
            "`search` is {} bytes long; at most {MAX_SEARCH_BYTES} are accepted.",
            search.len()
        ),
    )
    .with_details(json!({
        "field": SEARCH_FIELD,
        "maxBytes": MAX_SEARCH_BYTES,
        "actualBytes": search.len(),
    })))
}

/// Walks the whole type listing (T1-D1, T1-D7).
///
/// **All or nothing.** A page that fails fails the call: a silently short list makes real types
/// look nonexistent, which is strictly worse than an error the model can retry (T1 §9). Only
/// `limit` and the cursor are sent — nothing of the caller's — which is why a `400` here is
/// reported as ours.
async fn fetch_every_type(context: &CallContext) -> Result<Vec<ItdListEntry>, ToolError> {
    let engine = context.engine();

    paginate_all(|cursor| async move {
        engine
            .list_item_type_definitions::<ItdListEntry>(&ListQuery {
                limit: Some(MAX_LIMIT),
                cursor,
                ..ListQuery::default()
            })
            .await
            .map(|response| response.value)
    })
    .await
}

/// Projects one listed type into its row, or `None` when it has no served version.
///
/// A type with nothing served is **omitted** (T1-D4): nothing about it is addressable, and
/// listing it would invite a call that can only fail.
fn project(entry: ItdListEntry) -> Option<CatalogType> {
    let spec = entry.spec;
    let version = select_served_version(&spec.versions)?.name.clone();

    Some(CatalogType {
        kind: spec.names.kind,
        family: spec.names.plural,
        group: spec.group,
        version,
        display_name: spec.names.display_plural,
        // Verbatim or nothing (T1 §7): a blank description is absent, and there is no fallback.
        description: spec
            .llm_description
            .filter(|description| !description.trim().is_empty()),
        history_enabled: spec.history.is_some_and(|history| history.enabled),
    })
}

/// Whether `row` matches an already-lowercased `needle` (T1-D5, T1 §6).
///
/// A plain substring over the four fields, case-insensitive under **full Unicode** lowercasing —
/// `to_ascii_lowercase` would silently fail on accented text. The description searched is the
/// description returned, so nothing can match on text the caller cannot see.
fn matches_search(row: &CatalogType, needle: &str) -> bool {
    [
        Some(row.kind.as_str()),
        Some(row.family.as_str()),
        row.display_name.as_deref(),
        row.description.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().contains(needle))
}

#[cfg(test)]
mod tests;
