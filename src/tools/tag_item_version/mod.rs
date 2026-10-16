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
// `tag_item_version`: give one revision of an item a SemVer name, so it can be restored later.
// What cannot be tagged is said before any call; "now" is resolved and checked; a taken name is told
// apart from a repeat of the same tag, so a retry after a lost answer never becomes a second tag.

use crate::{
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::{
        apply_item::{MAX_KIND_BYTES, MAX_NAME_BYTES},
        arguments::validate_group,
        versions::{suggest_free, validate_version_name},
    },
};
use catalog_client::{
    EngineClient, ItemAddress, Remedy, ToolError, TypeCoordinates,
    error::codes,
    history::HistoryRefusal,
    is_valid_kind, is_valid_name,
    models::ItemVersion,
    pagination::{MAX_LIMIT, paginate_all},
    resolve_kind_or_suggest,
};
use chrono::DateTime;
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "tag_item_version";

/// What the tool does: the field that is not the API version, what is tagged by default, where it
/// works, and what a version survives — the part a model asked to "snapshot before deleting" must
/// not get wrong.
const TOOL_DESCRIPTION: &str = "Tags an item (`name`, `kind`, `group` for a shared kind) with a \
     SemVer `versionName` such as `1.4.0` — not the type's API version — so it can be restored later. \
     It tags the item as it stands, or `revision`, a uid from the item's history in describe_item. \
     Only for types with `historyEnabled` in list_catalog_types; AI Foundry items are versioned from \
     the AI Foundry. A name is used once per item: a taken one gets a free suggestion, and a repeat of \
     the same tag is recognised. Versions survive history trimming, not the item's deletion, and \
     cannot be deleted or renamed.";

/// The longest description, in bytes — input hygiene; the engine sets none.
pub const MAX_DESCRIPTION_BYTES: usize = 512;

/// How many existing names a taken name's error lists.
const MAX_LISTED_VERSIONS: usize = 20;

/// The AI Foundry's group, whose items the gateway lets only the AI Foundry write.
const AI_FOUNDRY_GROUP: &str = "ai.mia-platform.eu";

/// The name suffixes the gateway reserves for built-in items.
const BUILT_IN_SUFFIXES: [&str; 3] = [".mia-platform.eu", ".mia-care.io", ".mia-fintech.io"];

/// Said when the newest recorded revision is older than the item's last change.
const STALE_WARNING: &str = "The newest recorded revision is older than the item's last change; \
     history may be catching up. The tag is of that revision.";

/// Arguments for `tag_item_version`.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[serde(deny_unknown_fields)]
pub struct TagItemVersionInput {
    /// The item's name.
    #[serde(rename = "name")]
    pub name: String,

    /// The item's type.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// The SemVer name to give, such as `1.4.0`.
    #[serde(rename = "versionName")]
    pub version_name: String,

    /// What the version is.
    #[serde(rename = "description")]
    pub description: Option<String>,

    /// A revision uid; omitted, the item as it stands.
    #[serde(rename = "revision")]
    pub revision: Option<String>,
}

/// What a tag did, or found already done.
#[derive(Serialize, Default)]
struct Tagged {
    #[serde(rename = "tagged")]
    tagged: bool,

    /// `true` when this very tag already existed — a repeat, recognised.
    #[serde(rename = "alreadyTagged", skip_serializing_if = "std::ops::Not::not")]
    already_tagged: bool,

    #[serde(rename = "name")]
    name: String,

    #[serde(rename = "kind")]
    kind: String,

    #[serde(rename = "group")]
    group: String,

    #[serde(rename = "version")]
    version: String,

    #[serde(rename = "family")]
    family: String,

    #[serde(rename = "versionName")]
    version_name: String,

    #[serde(rename = "revision")]
    revision: String,

    /// `true` when this tool chose the revision: the item as it stood.
    #[serde(rename = "revisionResolved")]
    revision_resolved: bool,

    /// The state's time, when this tool read it.
    #[serde(rename = "revisionRecordedAt", skip_serializing_if = "Option::is_none")]
    revision_recorded_at: Option<String>,

    #[serde(rename = "taggedAt", skip_serializing_if = "Option::is_none")]
    tagged_at: Option<String>,

    /// The tagger's name, or its id when the name has not resolved.
    #[serde(rename = "taggedBy", skip_serializing_if = "Option::is_none")]
    tagged_by: Option<String>,

    #[serde(rename = "taggedById", skip_serializing_if = "Option::is_none")]
    tagged_by_id: Option<String>,

    /// On a recognised repeat: whether the existing tag is this caller's.
    #[serde(rename = "taggedByYou", skip_serializing_if = "Option::is_none")]
    tagged_by_you: Option<bool>,
}

/// The revision to tag, and what is known of it.
struct Target {
    revision: String,
    resolved: bool,
    recorded_at: Option<String>,
    stale: bool,
}

/// `tag_item_version` — tag a revision of an item with a SemVer name.
pub struct TagItemVersion;

impl Tool for TagItemVersion {
    type Input = TagItemVersionInput;

    /// Not destructive: it adds a version and changes nothing else.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<TagItemVersionInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().destructive(false),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;
        let engine = context.engine();

        let coordinates =
            resolve_kind_or_suggest(engine, &input.kind, input.group.as_deref()).await?;

        // What cannot record is known from the type itself: no call is spent on it.
        if let Some(refusal) = HistoryRefusal::of_type(
            &coordinates.group,
            &coordinates.family,
            coordinates.history_enabled,
        ) {
            return Err(refusal.to_error(&coordinates.kind));
        }

        let address = ItemAddress::new(
            &coordinates.group,
            &coordinates.version,
            &coordinates.family,
            &input.name,
        )?;

        let target = match &input.revision {
            Some(revision) => Target {
                revision: revision.clone(),
                resolved: false,
                recorded_at: None,
                stale: false,
            },
            None => resolve_now(engine, &address).await?,
        };

        let outcome = engine
            .create_version(
                &address,
                &input.version_name,
                &target.revision,
                input.description.as_deref(),
            )
            .await;

        let answer = match outcome {
            Ok(response) => tagged(&input, &coordinates, &target, &response.value, None),
            Err(error) => refused(engine, &input, &coordinates, &address, &target, error).await?,
        };

        let output = serde_json::to_value(answer)
            .map(ToolOutput::new)
            .map_err(|err| {
                ToolError::new(
                    codes::SERVER_DEFECT,
                    Remedy::Escalate,
                    format!("The tag's answer could not be rendered: {err}"),
                )
            })?;

        Ok(if target.stale {
            output.with_warning(STALE_WARNING)
        } else {
            output
        })
    }
}

/// "Now": the newest revision, checked against the item's last change.
async fn resolve_now(engine: &EngineClient, address: &ItemAddress) -> Result<Target, ToolError> {
    let page = engine.list_revisions(address, 1).await?.value;
    let Some(newest) = page.items.into_iter().next() else {
        return Err(ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            "The item has no recorded revision yet, so there is nothing to tag.",
        )
        .with_next_step("write the item — any change records the revision this would tag"));
    };

    let item = engine.get_item(address).await?.value;
    let stale = match (
        item.metadata.update_timestamp.as_deref().and_then(instant),
        instant(&newest.metadata.creation_timestamp),
    ) {
        (Some(updated), Some(recorded)) => updated > recorded,
        _ => false,
    };

    Ok(Target {
        revision: newest.metadata.uid,
        resolved: true,
        recorded_at: Some(newest.metadata.creation_timestamp),
        stale,
    })
}

/// An RFC 3339 time, compared as an instant.
fn instant(text: &str) -> Option<DateTime<chrono::Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&chrono::Utc))
}

/// The answer for a version — the one just created, or the one found already tagging the revision.
fn tagged(
    input: &TagItemVersionInput,
    coordinates: &TypeCoordinates,
    target: &Target,
    version: &ItemVersion,
    caller: Option<Option<&str>>,
) -> Tagged {
    let author = version.metadata.author.as_ref();
    let repeat = caller.is_some();

    Tagged {
        tagged: !repeat,
        already_tagged: repeat,
        name: input.name.clone(),
        kind: coordinates.kind.clone(),
        group: coordinates.group.clone(),
        version: coordinates.version.clone(),
        family: coordinates.family.clone(),
        version_name: version.metadata.name.clone(),
        revision: target.revision.clone(),
        revision_resolved: target.resolved,
        revision_recorded_at: target.recorded_at.clone(),
        tagged_at: version.metadata.creation_timestamp.clone(),
        tagged_by: author.map(|author| {
            author
                .display_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| author.id.clone())
        }),
        tagged_by_id: author.map(|author| author.id.clone()),
        tagged_by_you: caller
            .flatten()
            .zip(author)
            .map(|(caller, author)| caller == author.id),
    }
}

/// What a refused tag tells the model — or, for a repeat of this very tag, the answer it would have
/// had.
async fn refused(
    engine: &EngineClient,
    input: &TagItemVersionInput,
    coordinates: &TypeCoordinates,
    address: &ItemAddress,
    target: &Target,
    error: ToolError,
) -> Result<Tagged, ToolError> {
    match error.code {
        codes::CONFLICT => on_conflict(engine, input, coordinates, address, target, error).await,
        codes::NOT_FOUND => Err(on_not_found(engine, address, target, error).await),
        codes::UNSUPPORTED_FOR_TYPE => Err(HistoryRefusal::of_engine_message(&error.message)
            .map_or(error, |refusal| refusal.to_error(&coordinates.kind))),
        codes::FORBIDDEN => Err(on_forbidden(input, coordinates, error)),
        codes::UNKNOWN_OUTCOME => Err(error.with_next_step(
            "call again with the same arguments; a repeat of this tag is recognised",
        )),
        _ => Err(error),
    }
}

/// A taken name: the same tag again — recognised as done — or a clash, with the nearest free name.
async fn on_conflict(
    engine: &EngineClient,
    input: &TagItemVersionInput,
    coordinates: &TypeCoordinates,
    address: &ItemAddress,
    target: &Target,
    error: ToolError,
) -> Result<Tagged, ToolError> {
    let versions = paginate_all(|cursor| async move {
        engine
            .list_versions(address, MAX_LIMIT, cursor.as_ref())
            .await
            .map(|response| response.value)
    })
    .await;

    let mut details = error
        .details
        .as_deref()
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let message = format!(
        "`{}` already names another version of this item, and a version is never renamed: tag with \
         a different `versionName`.",
        input.version_name
    );

    let versions = match versions {
        Ok(versions) => versions,
        Err(listing_error) => {
            tracing::warn!(error = %listing_error, "the item's versions could not be listed");
            return Err(ToolError {
                message,
                details: (!details.is_empty())
                    .then(|| Box::new(serde_json::Value::Object(details))),
                ..clash(error)
            });
        }
    };

    if let Some(same) = versions.iter().find(|version| {
        version.metadata.name == input.version_name
            && version
                .spec
                .as_ref()
                .and_then(|spec| spec.revision_id.as_deref())
                == Some(target.revision.as_str())
    }) {
        let caller = engine.identity().principal_id();
        return Ok(tagged(input, coordinates, target, same, Some(caller)));
    }

    let names: Vec<String> = versions
        .iter()
        .map(|version| version.metadata.name.clone())
        .collect();
    if let Some(suggestion) = suggest_free(&input.version_name, &names) {
        details.insert("suggestedVersion".to_string(), json!(suggestion));
    }
    details.insert(
        "existing".to_string(),
        json!(names.iter().take(MAX_LISTED_VERSIONS).collect::<Vec<_>>()),
    );
    details.insert("existingCount".to_string(), json!(names.len()));

    Err(ToolError {
        message,
        details: Some(Box::new(serde_json::Value::Object(details))),
        ..clash(error)
    })
}

/// A name clash: the input must change; waiting never helps.
fn clash(error: ToolError) -> ToolError {
    ToolError {
        code: codes::CONFLICT,
        remedy: Remedy::RetryAfterChange,
        next_step: None,
        ..error
    }
}

/// A `404`, told apart by a read rather than by its wording.
async fn on_not_found(
    engine: &EngineClient,
    address: &ItemAddress,
    target: &Target,
    error: ToolError,
) -> ToolError {
    if target.resolved {
        return error.with_next_step("call again: the item or its history changed in between");
    }

    match engine.get_item(address).await {
        Err(read) if read.code == codes::NOT_FOUND => ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            "No such item of that type.",
        )
        .with_next_step("find the item and its kind with describe_item"),
        _ => ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            format!(
                "Revision `{}` is not one of this item's: it is wrong, or it was trimmed.",
                target.revision
            ),
        )
        .with_next_step(
            "omit `revision` to tag the item as it stands, or take a uid from the item's history",
        ),
    }
}

/// The gateway's `403`, which has no body: the item is the only way to say why.
fn on_forbidden(
    input: &TagItemVersionInput,
    coordinates: &TypeCoordinates,
    error: ToolError,
) -> ToolError {
    let lowered = input.name.to_lowercase();

    let message = if coordinates.group.eq_ignore_ascii_case(AI_FOUNDRY_GROUP) {
        "AI Foundry items are versioned only from the AI Foundry, or with a permission this caller \
         lacks."
    } else if BUILT_IN_SUFFIXES
        .iter()
        .any(|suffix| lowered.ends_with(suffix))
    {
        "Built-in items cannot be tagged."
    } else {
        return error;
    };

    ToolError {
        message: message.to_string(),
        ..error
    }
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &TagItemVersionInput) -> Result<(), ToolError> {
    if input.name.is_empty() || input.name.len() > MAX_NAME_BYTES || !is_valid_name(&input.name) {
        return Err(invalid(
            "name",
            format!(
                "`{}` is not an item name: lowercase letters, digits, `-` and `.`, starting and \
                 ending with a letter or digit.",
                input.name
            ),
        ));
    }

    if input.kind.is_empty() || input.kind.len() > MAX_KIND_BYTES || !is_valid_kind(&input.kind) {
        return Err(invalid(
            "kind",
            format!(
                "`{}` is not a kind: a kind is a letter followed by letters and digits.",
                input.kind
            ),
        )
        .with_next_step("call list_catalog_types to see the kinds that exist"));
    }

    validate_group(input.group.as_deref(), true)?;
    validate_version_name(&input.version_name)?;

    if let Some(revision) = &input.revision
        && uuid::Uuid::parse_str(revision).is_err()
    {
        return Err(invalid(
            "revision",
            format!("`{revision}` is not a revision uid."),
        ));
    }

    if let Some(description) = &input.description
        && description.len() > MAX_DESCRIPTION_BYTES
    {
        return Err(invalid(
            "description",
            format!(
                "`description` is {} bytes and takes at most {MAX_DESCRIPTION_BYTES}.",
                description.len()
            ),
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
