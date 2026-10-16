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
// `restore_item_version`: replace an item's content with one of its tagged versions. The restore
// is guarded on the state this tool read, says which fields it replaced, and names the revision
// holding the state before — only after checking that the revision is that state.

use crate::{
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::{
        apply_item::{MAX_KIND_BYTES, MAX_NAME_BYTES},
        arguments::validate_group,
        versions::{VersionWrite, explain_forbidden, validate_version_name},
    },
};
use catalog_client::{
    EngineClient, ItemAddress, Remedy, ToolError, TypeCoordinates,
    error::codes,
    history::HistoryRefusal,
    is_valid_kind, is_valid_name,
    models::ItemRevision,
    ops::compliance::COMPLIANCE_GROUP,
    pagination::{MAX_LIMIT, paginate_all},
    resolve_kind_or_suggest,
    write::changed_paths,
};
use chrono::{DateTime, Utc};
use rmcp::model::ToolAnnotations;
use serde::Serialize;
use serde_json::{Map, Value, json};

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "restore_item_version";

/// What the tool does, starting with what it replaces — never in the words of the tools that keep
/// what they were not told.
const TOOL_DESCRIPTION: &str = "Replaces an item's spec, metadata (owner and followers included) \
     and custom fields with a tagged version's — `versionName` (SemVer, not the API version) of the \
     item `name`, `kind`, `group` for a shared kind; anything changed since is lost. To be able to \
     undo, tag the item first with tag_item_version (versions are permanent: a name such as \
     `1.4.0-undo.1`). Returns the `changed` paths (`noOp` when none), a `note` when custom fields, \
     owner or followers changed, and `previousRevision` when verified: to undo, pass it to \
     tag_item_version as `revision`, then restore that. Version names are in describe_item's \
     versions. Not for Campaigns (use apply_item) or types without `historyEnabled`; a deleted item \
     cannot be restored.";

/// The kind whose scheduled jobs a restore would leave behind.
const CAMPAIGN_KIND: &str = "Campaign";

/// How many existing names a missing version's error lists.
const MAX_LISTED_VERSIONS: usize = 20;

/// The newest revisions read after a restore: its own, and the one before it.
const REVISIONS_READ: u32 = 2;

/// The `metadata` fields a restore writes. Everything else in `metadata` is identity or the
/// server's own.
const WRITTEN_METADATA_FIELDS: [&str; 8] = [
    "title",
    "description",
    "labels",
    "annotations",
    "links",
    "tags",
    "owner",
    "followers",
];

/// The top-level fields a restore writes, besides `metadata`.
const WRITTEN_FIELDS: [&str; 2] = ["spec", "customFields"];

/// The changed paths no other tool here writes, which a restore changing them says aloud.
const NOTED_PREFIXES: [(&str, &str); 3] = [
    ("customFields", "custom fields"),
    ("metadata.owner", "the owner"),
    ("metadata.followers", "the followers"),
];

/// The engine's prefixes for a version that no longer fits the type's rules.
const SCHEMA_PREFIXES: [&str; 2] = [
    "Body does not conform to Item Type Definition schema",
    "failed to deserialize spec",
];

/// How the engine starts a rule's own validation failure — `Rule is invalid: …`, or with the
/// rule's name — and the words that follow it.
const RULE_PREFIX: &str = "Rule ";

/// See [`RULE_PREFIX`].
const RULE_INVALID: &str = " is invalid";

/// The engine's prefixes for a version holding a custom field that no longer applies or validates.
const CUSTOM_FIELD_PREFIXES: [&str; 2] = [
    "Restored custom field key",
    "Restored value for custom field",
];

/// The engine's prefix for a `404` about the type rather than the item or the version.
const TYPE_NOT_FOUND_PREFIX: &str = "Resource of family";

/// Said on every restore: the one re-evaluation an update runs that a restore does not.
const CAMPAIGNS_WARNING: &str = "A restore does not re-evaluate the campaigns that include this \
     item; run_compliance_evaluation re-runs one.";

/// Said when the revision holding the state before is gone.
const TRIMMED_WARNING: &str = "The state before this restore was not kept: the type's history \
     retention removed it. Tag the item with tag_item_version before a restore to keep it.";

/// Said when another write followed the restore before it could be checked.
const OVERTAKEN_WARNING: &str = "Another write followed this restore before its history could be \
     checked, so `previousRevision` is not given; see the item's history in describe_item.";

/// Said when the history could not be read after the restore.
const UNREAD_WARNING: &str = "The restore succeeded, but the item's history could not be read \
     after it, so `previousRevision` is not given; see the item's history in describe_item.";

/// Said on a restore that changed no content.
const NO_OP_NOTE: &str = "The content was already this version's and is unchanged. The write \
     still happened: the item's update time moved and the change was announced.";

/// Arguments for `restore_item_version`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[serde(deny_unknown_fields)]
pub struct RestoreItemVersionInput {
    /// The item's name.
    #[serde(rename = "name")]
    pub name: String,

    /// The item's type.
    #[serde(rename = "kind")]
    pub kind: String,

    /// The kind's group, if several types share it.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// The version's SemVer name, such as `1.4.0`.
    #[serde(rename = "versionName")]
    pub version_name: String,
}

/// What a restore did.
#[derive(Serialize)]
struct Restored {
    #[serde(rename = "restored")]
    restored: bool,

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

    /// The dotted paths the restore changed, sorted.
    #[serde(rename = "changed")]
    changed: Vec<String>,

    #[serde(rename = "noOp")]
    no_op: bool,

    /// The revision holding the state before — only when checked to be it.
    #[serde(rename = "previousRevision", skip_serializing_if = "Option::is_none")]
    previous_revision: Option<String>,

    #[serde(rename = "note", skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// What the history says about the state before the restore.
enum Previous {
    /// The revision holding it, checked.
    Verified(String),

    /// Nothing to name, for the reason the warning gives.
    Omitted(String),

    /// Nothing to name, and nothing to say.
    None,
}

/// `restore_item_version` — replace an item's content with one of its versions.
pub struct RestoreItemVersion;

impl Tool for RestoreItemVersion {
    type Input = RestoreItemVersionInput;

    /// No hint is emitted: destructive by default, which drives the client's confirmation, and not
    /// idempotent — a repeat changes no content but is written and announced again.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<RestoreItemVersionInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new(),
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

        if coordinates.kind == CAMPAIGN_KIND && coordinates.group == COMPLIANCE_GROUP {
            return Err(campaign_refusal());
        }

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

        let before = match engine.get_item_raw(&address).await {
            Ok(response) => response.value,
            Err(error) if error.code == codes::NOT_FOUND => return Err(no_such_item()),
            Err(error) => return Err(error),
        };
        let resource_version = before.get("resourceVersion").and_then(Value::as_str);

        let after = match engine
            .restore_version(&address, &input.version_name, resource_version)
            .await
        {
            Ok(response) => response.value,
            Err(error) => return Err(refused(engine, &input, &coordinates, &address, error).await),
        };

        let changed = changed_paths(&written(&before), &written(&after));
        let no_op = changed.is_empty();

        let previous = previous_state(
            engine,
            &address,
            &input.version_name,
            &before,
            &after,
            no_op,
        )
        .await;

        let note = if no_op {
            Some(NO_OP_NOTE.to_string())
        } else {
            noted(&changed)
        };

        let (previous_revision, warning) = match previous {
            Previous::Verified(uid) => (Some(uid), None),
            Previous::Omitted(warning) => (None, Some(warning)),
            Previous::None => (None, None),
        };

        let answer = Restored {
            restored: true,
            name: input.name,
            kind: coordinates.kind,
            group: coordinates.group,
            version: coordinates.version,
            family: coordinates.family,
            version_name: input.version_name,
            changed,
            no_op,
            previous_revision,
            note,
        };

        let output = serde_json::to_value(answer)
            .map(ToolOutput::new)
            .map_err(|err| {
                ToolError::new(
                    codes::SERVER_DEFECT,
                    Remedy::Escalate,
                    format!("The restore's answer could not be rendered: {err}"),
                )
            })?
            .with_warning(CAMPAIGNS_WARNING);

        Ok(match warning {
            Some(warning) => output.with_warning(warning),
            None => output,
        })
    }
}

/// The part of an item a restore writes — the rest is identity or the server's own, and is never
/// compared.
fn written(manifest: &Value) -> Value {
    let mut kept = Map::new();

    for field in WRITTEN_FIELDS {
        if let Some(value) = manifest.get(field) {
            kept.insert(field.to_string(), value.clone());
        }
    }

    if let Some(Value::Object(metadata)) = manifest.get("metadata") {
        let metadata: Map<String, Value> = WRITTEN_METADATA_FIELDS
            .iter()
            .filter_map(|field| {
                metadata
                    .get(*field)
                    .map(|value| ((*field).to_string(), value.clone()))
            })
            .collect();
        kept.insert("metadata".to_string(), Value::Object(metadata));
    }

    Value::Object(kept)
}

/// The note for changes no other tool here makes.
fn noted(changed: &[String]) -> Option<String> {
    let touched: Vec<&str> = NOTED_PREFIXES
        .iter()
        .filter(|(prefix, _)| {
            changed.iter().any(|path| {
                path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
        .map(|(_, words)| *words)
        .collect();

    let list = match touched.as_slice() {
        [] => return None,
        [one] => (*one).to_string(),
        [first, second] => format!("{first} and {second}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    };

    Some(format!("This restore also changed {list}."))
}

/// What the history says about the state before the restore. Never fails the call: a read that
/// fails, or comes too late, only means nothing is named.
async fn previous_state(
    engine: &EngineClient,
    address: &ItemAddress,
    version_name: &str,
    before: &Value,
    after: &Value,
    no_op: bool,
) -> Previous {
    let newest = match engine.list_revisions(address, REVISIONS_READ).await {
        Ok(response) => response.value.items,
        Err(error) => {
            tracing::warn!(%error, "the item's revisions could not be read after a restore");
            return Previous::Omitted(UNREAD_WARNING.to_string());
        }
    };

    let restored_this = |revision: &ItemRevision| {
        revision
            .spec
            .as_ref()
            .and_then(|spec| spec.restored_from.as_deref())
            == Some(version_name)
    };

    if no_op {
        // Nothing new was recorded. A restore of this same version on top means an earlier one
        // landed — perhaps an attempt whose answer was lost.
        return match newest.first() {
            Some(top) if restored_this(top) => Previous::Omitted(format!(
                "The newest revision, `{}`, is already a restore of `{version_name}`: an earlier \
                 restore landed. The state before it is the revision below it in the item's \
                 history; this call cannot check it.",
                top.metadata.uid
            )),
            _ => Previous::None,
        };
    }

    let mut revisions = newest.into_iter();
    match revisions.next() {
        Some(top) if restored_this(&top) && recorded_by_this_write(&top, after) => {}
        _ => return Previous::Omitted(OVERTAKEN_WARNING.to_string()),
    }

    let Some(below) = revisions.next() else {
        return Previous::Omitted(TRIMMED_WARNING.to_string());
    };

    match engine.get_revision(address, &below.metadata.uid).await {
        Ok(response) if changed_paths(&written(before), &written(&response.value)).is_empty() => {
            Previous::Verified(below.metadata.uid)
        }
        Ok(_) => Previous::Omitted(TRIMMED_WARNING.to_string()),
        Err(error) if error.code == codes::NOT_FOUND => {
            Previous::Omitted(TRIMMED_WARNING.to_string())
        }
        Err(error) => {
            tracing::warn!(%error, "the revision before a restore could not be read");
            Previous::Omitted(UNREAD_WARNING.to_string())
        }
    }
}

/// Whether `revision` was recorded by the write that produced `after`: not before it.
fn recorded_by_this_write(revision: &ItemRevision, after: &Value) -> bool {
    let updated = after
        .pointer("/metadata/updateTimestamp")
        .and_then(Value::as_str)
        .and_then(instant);

    match (instant(&revision.metadata.creation_timestamp), updated) {
        (Some(recorded), Some(updated)) => recorded >= updated,
        _ => false,
    }
}

/// An RFC 3339 time, compared as an instant.
fn instant(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// What a refused restore tells the model.
async fn refused(
    engine: &EngineClient,
    input: &RestoreItemVersionInput,
    coordinates: &TypeCoordinates,
    address: &ItemAddress,
    error: ToolError,
) -> ToolError {
    match error.code {
        codes::NOT_FOUND => on_not_found(engine, input, address, error).await,
        codes::INVALID_INPUT => on_bad_request(error),
        codes::CONFLICT => ToolError {
            message: "The item changed after this call read it, so nothing was restored."
                .to_string(),
            ..error
        }
        .with_next_step(
            "the item changed after this call read it: check it with describe_item, then call \
             again if the restore is still wanted",
        ),
        codes::UNSUPPORTED_FOR_TYPE => HistoryRefusal::of_engine_message(&error.message)
            .map_or(error, |refusal| refusal.to_error(&coordinates.kind)),
        codes::FORBIDDEN => explain_forbidden(
            VersionWrite::Restore,
            &input.name,
            &coordinates.group,
            error,
        ),
        codes::UNKNOWN_OUTCOME => error.with_next_step(
            "check the item with describe_item and compare it with the version; calling again \
             re-reads first, and a repeat names the earlier restore",
        ),
        _ => error,
    }
}

/// The restore's `404`: the item has just been read, so it is the version — unless the engine says
/// it is the type.
async fn on_not_found(
    engine: &EngineClient,
    input: &RestoreItemVersionInput,
    address: &ItemAddress,
    error: ToolError,
) -> ToolError {
    if error.message.starts_with(TYPE_NOT_FOUND_PREFIX) {
        return error;
    }

    let versions = paginate_all(|cursor| async move {
        engine
            .list_versions(address, MAX_LIMIT, cursor.as_ref())
            .await
            .map(|response| response.value)
    })
    .await;

    let versions = match versions {
        Ok(versions) => versions,
        Err(listing_error) => {
            tracing::warn!(error = %listing_error, "the item's versions could not be listed");
            return ToolError::new(
                codes::NOT_FOUND,
                Remedy::RetryAfterChange,
                format!(
                    "No version `{}` of this item was found, and its versions could not be \
                     listed: the item may be gone.",
                    input.version_name
                ),
            )
            .with_next_step("check the item with describe_item");
        }
    };

    if versions.is_empty() {
        return ToolError::new(
            codes::NOT_FOUND,
            Remedy::RetryAfterChange,
            "This item has no versions, so there is nothing to restore.",
        )
        .with_next_step("tag one with tag_item_version");
    }

    let names: Vec<&str> = versions
        .iter()
        .map(|version| version.metadata.name.as_str())
        .collect();

    ToolError::new(
        codes::NOT_FOUND,
        Remedy::RetryAfterChange,
        format!("This item has no version `{}`.", input.version_name),
    )
    .with_details(json!({
        "availableVersions": names.iter().take(MAX_LISTED_VERSIONS).collect::<Vec<_>>(),
        "availableCount": names.len(),
    }))
    .with_next_step("restore one of `availableVersions`")
}

/// A `400`, told apart by its source: the type's rules, a custom field, or anything else, relayed.
fn on_bad_request(error: ToolError) -> ToolError {
    let engine_message = error.message.clone();

    let (message, next_step) = if SCHEMA_PREFIXES
        .iter()
        .any(|prefix| engine_message.starts_with(prefix))
        || (engine_message.starts_with(RULE_PREFIX) && engine_message.contains(RULE_INVALID))
    {
        (
            "This version no longer fits the type's rules as they are now, so it cannot be \
             restored.",
            "restore another version, or see the current rules with get_item_schema",
        )
    } else if CUSTOM_FIELD_PREFIXES
        .iter()
        .any(|prefix| engine_message.starts_with(prefix))
    {
        (
            "This version holds a custom field that was deleted, no longer applies to this type, \
             or is no longer valid, so it cannot be restored.",
            "restore another version; patch_item_custom_fields with empty `fields` lists the keys \
             this item can take",
        )
    } else {
        return error;
    };

    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "engineMessage": engine_message }))
        .with_next_step(next_step)
}

/// The pre-read's `404`: nothing to restore — and nothing to undelete.
fn no_such_item() -> ToolError {
    ToolError::new(
        codes::NOT_FOUND,
        Remedy::RetryAfterChange,
        "No such item of that type — and a deleted item's versions are gone with it.",
    )
    .with_next_step("find the item and its kind with describe_item")
}

/// A Campaign: its scheduled jobs follow only the ordinary write.
fn campaign_refusal() -> ToolError {
    ToolError::new(
        codes::UNSUPPORTED_FOR_TYPE,
        Remedy::Escalate,
        "A restored campaign would keep the scheduled jobs of its current state, so campaigns are \
         not restored.",
    )
    .with_details(json!({ "kind": CAMPAIGN_KIND, "permanent": true }))
    .with_next_step("change its fields with apply_item, which reschedules them")
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: &RestoreItemVersionInput) -> Result<(), ToolError> {
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
    validate_version_name(&input.version_name)
}

/// An `invalid_input` naming the offending parameter.
fn invalid(parameter: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": parameter }))
}

#[cfg(test)]
mod tests;
