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
    select_served_version,
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "list_catalog_types";

/// What the tool does.
///
/// Note what it omits: nothing about `group`/`version`/`family` being needed elsewhere, no
/// pagination instructions. If the model had to be told any of that, the tool set would have
/// failed.
const TOOL_DESCRIPTION: &str = "Lists every item type in the catalog, with the coordinates needed \
     to address items of that type. Call this first when you do not already know a type's exact \
     `kind`. Returns every type in one response — there is no pagination. Use `search` to \
     narrow by name or purpose; `filteredFrom` is how many types there were before. \
     `hasLlmDescription`: read its briefing with get_item_schema `fields: []`.";

/// The longest `search` term accepted, in bytes.
///
/// Nothing legitimate needs more, and it keeps the filter's cost bounded.
pub const MAX_SEARCH_BYTES: usize = 200;

/// The input field `search`, as named in errors.
const SEARCH_FIELD: &str = "search";

/// Arguments for `list_catalog_types`.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct ListCatalogTypesInput {
    /// Case-insensitive substring, matched over kind, family, display name and description.
    //
    // No `#[serde(default)]`: an absent `Option` is already `None`, and the attribute would put
    // `"default": null` into the schema every `tools/list` pays for.
    #[serde(rename = "search")]
    pub search: Option<String>,
}

/// One row of the response. Field order here is the serialised order.
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

    /// The served version chosen by `catalog-client`'s rule.
    #[serde(rename = "version")]
    pub version: String,

    /// `spec.names.displayPlural`, when the type declares one.
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// `metadata.description`, the type's short description, **verbatim** — omitted when absent or
    /// blank, and never synthesised from anything else.
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Whether the type has an agent briefing, `spec.llmDescription`. Only the flag is listed:
    /// a briefing can be long, and a listing would carry every type's at once, so its text is read
    /// one type at a time, with `get_item_schema`.
    #[serde(
        rename = "hasLlmDescription",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub has_llm_description: bool,

    /// `"llmDescription"` when a `search` matched this type **only** in its briefing — the one text
    /// searched that the row does not show — so the caller can tell why it came back. Omitted
    /// otherwise, and always without a `search`.
    #[serde(rename = "matchedOn", skip_serializing_if = "Option::is_none")]
    pub matched_on: Option<&'static str>,

    /// `spec.history.enabled`, defaulting to `false`.
    #[serde(rename = "historyEnabled")]
    pub history_enabled: bool,
}

/// The whole response.
///
/// `search` and `filteredFrom` are present **whenever a search was supplied**, so an empty
/// result can always be told apart: *"nothing matched your term"* is not *"you have no types"*,
/// and echoing the term back is what lets the model notice its own typo.
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

/// `list_catalog_types` — every item type the caller can see, with its coordinates.
///
/// One engine listing, walked to exhaustion, projected, optionally filtered, sorted. Nothing is
/// cached: every call fetches the caller's own catalogue, which is what makes it
/// impossible for one tenant to be answered with another's.
pub struct ListCatalogTypes;

impl Tool for ListCatalogTypes {
    type Input = ListCatalogTypesInput;

    /// `readOnlyHint: true`; every other hint is the specification's default.
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
        // Validated at the boundary, before anything reaches the engine.
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
        let mut types: Vec<Listed> = entries.into_iter().filter_map(project).collect();

        // The evidence for revisiting the version-selection rule. `omitted` is zero everywhere
        // today; a non-zero value is the first sign the rule has started firing.
        tracing::debug!(
            fetched,
            omitted = fetched - types.len(),
            "listed the catalog's item types"
        );

        // By `kind`, byte-wise; `group` then `family` break the tie two groups sharing
        // a kind would otherwise leave to the engine's unspecified order.
        types.sort_by(|left, right| {
            (&left.row.kind, &left.row.group, &left.row.family).cmp(&(
                &right.row.kind,
                &right.row.group,
                &right.row.family,
            ))
        });

        let output = match input.search {
            None => ListCatalogTypesOutput {
                total: types.len(),
                types: types.into_iter().map(|listed| listed.row).collect(),
                search: None,
                filtered_from: None,
            },
            Some(search) => {
                let filtered_from = types.len();
                let needle = search.to_lowercase();
                let types: Vec<CatalogType> = types
                    .into_iter()
                    .filter_map(|listed| matched(listed, &needle))
                    .collect();

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

/// Rejects an empty or blank `search`, and one longer than [`MAX_SEARCH_BYTES`].
///
/// An empty term is a substring of every text, so it would silently mean *"every type"* — which is
/// what leaving `search` out already says, and the model should say it that way.
fn validate_search(search: &str) -> Result<(), ToolError> {
    if search.trim().is_empty() {
        return Err(ToolError::new(
            codes::INVALID_INPUT,
            Remedy::RetryAfterChange,
            "`search` is empty: omit it to list every type, or give a word to narrow by.",
        )
        .with_details(json!({ "field": SEARCH_FIELD })));
    }

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

/// Walks the whole type listing — all or nothing, so a failing page fails the
/// call. Only `limit` and the cursor are sent, nothing of the caller's, which is why a `400`
/// here is reported as ours.
async fn fetch_every_type(context: &CallContext) -> Result<Vec<ItdListEntry>, ToolError> {
    context
        .engine()
        .list_all_item_type_definitions::<ItdListEntry>()
        .await
}

/// A row, and the briefing it is searched by but does not show.
struct Listed {
    row: CatalogType,
    llm_description: Option<String>,
}

/// The value of `matchedOn` for a row found only in its briefing.
const MATCHED_ON_LLM_DESCRIPTION: &str = "llmDescription";

/// Projects one listed type into its row, or `None` when it has no served version.
///
/// A type with nothing served is **omitted**: nothing about it is addressable, and
/// listing it would invite a call that can only fail.
fn project(entry: ItdListEntry) -> Option<Listed> {
    let spec = entry.spec;
    let version = select_served_version(&spec.versions)?.name.clone();

    // Verbatim or nothing, for both: a blank text is absent, and neither stands in for the other.
    let llm_description = non_blank(spec.llm_description);

    Some(Listed {
        row: CatalogType {
            kind: spec.names.kind,
            family: spec.names.plural,
            group: spec.group,
            version,
            display_name: spec.names.display_plural,
            description: non_blank(entry.metadata.description),
            has_llm_description: llm_description.is_some(),
            matched_on: None,
            history_enabled: spec.history.is_some_and(|history| history.enabled),
        },
        llm_description,
    })
}

/// `text`, unless it is absent or only whitespace.
fn non_blank(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

/// The row, if `listed` matches the already-lowercased `needle` — marked when only its briefing,
/// which the row does not show, did.
fn matched(listed: Listed, needle: &str) -> Option<CatalogType> {
    let Listed {
        mut row,
        llm_description,
    } = listed;

    if matches_search(&row, needle) {
        return Some(row);
    }

    llm_description
        .is_some_and(|briefing| contains(&briefing, needle))
        .then(|| {
            row.matched_on = Some(MATCHED_ON_LLM_DESCRIPTION);
            row
        })
}

/// Whether the texts `row` shows match an already-lowercased `needle`.
///
/// A plain substring over `kind`, `family`, `displayName` and the short `description`,
/// case-insensitive under **full Unicode** lowercasing — `to_ascii_lowercase` would silently fail
/// on accented text. The briefing is searched too, by [`matched`], which marks a row found only
/// there.
fn matches_search(row: &CatalogType, needle: &str) -> bool {
    [
        Some(row.kind.as_str()),
        Some(row.family.as_str()),
        row.display_name.as_deref(),
        row.description.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| contains(field, needle))
}

/// Whether `text`, lowercased, contains the already-lowercased `needle`.
fn contains(text: &str, needle: &str) -> bool {
    text.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests;
