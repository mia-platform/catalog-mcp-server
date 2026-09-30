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
    address::{ItemAddress, ItemTypeAddress},
    client::{EngineClient, EngineResponse},
    error::{Remedy, ToolError, codes},
    warning::EngineWarning,
};
use serde_json::{Map, Value};

/// Fields a `PUT` ignores, which this server therefore strips before sending (§8.5).
///
/// `customFields` is the one that matters: the engine ignores it on `PUT` and says so in a
/// `Warning`, so echoing it back to the model would report a write that did not happen.
const SERVER_OWNED_FIELDS: &[&str] = &["customFields", "resourceVersion"];

/// Read-only metadata the engine assigns and a write must not carry back.
const READ_ONLY_METADATA_FIELDS: &[&str] = &[
    "uid",
    "urn",
    "creationTimestamp",
    "updateTimestamp",
    "family",
];

/// Where the optimistic-concurrency token goes on a given endpoint (§8.5).
///
/// Items and Item Type Definitions carry it **in the body**; custom fields and restore take it
/// as a **query parameter**. Hiding the difference behind an enum is what stops six tools each
/// remembering which is which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceVersionIn {
    /// `PUT` on an item or an Item Type Definition.
    Body,

    /// `PATCH …/custom-fields` and `POST …/restore`.
    Query,
}

/// What to do about a `409` (P7, D23).
///
/// **The rule is stated once and encoded, not written down for six tools to re-derive:** retry
/// is permitted only when the intent is independent of the state it lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Re-read and re-apply **once**. For merge-patch intents — `apply_item`,
    /// `patch_item_custom_fields` — where "set these fields" means the same thing whatever else
    /// changed underneath.
    RetryOnce,

    /// Report the conflict. For deletes, restores and type writes, where the caller's intent
    /// was formed against the state they read and landing it on a different one is not the same
    /// act.
    Report,
}

/// What the caller expects to find before it writes — so a create cannot quietly become an update
/// of something else, nor an update of a definition deleted a moment ago quietly become a create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existence {
    /// Create or update, whichever the read finds (`apply_item`).
    Any,

    /// The object must exist: a `404` on the read is returned as it is, and nothing is written.
    Present,

    /// The object must not exist: finding one is a `conflict`, and nothing is written.
    Absent,
}

/// A field path inside a manifest, as `changed` reports it.
pub type FieldPath = String;

/// What a write did (§8.5).
///
/// `changed` is computed by diffing the pre-read against the result, which is what lets a tool
/// report a no-op honestly instead of claiming a write it did not make.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WriteOutcome {
    /// Whether the object did not exist and was created.
    pub created: bool,

    /// Which field paths actually differ, in document order.
    pub changed: Vec<FieldPath>,

    /// Whether the request was retried — always visible, never inferred (§8.1).
    pub retried: bool,

    /// Whatever the engine warned about.
    pub warnings: Vec<EngineWarning>,

    /// The object as the last read found it; `None` on a create. For a tool whose report compares
    /// before and after (T12).
    pub before: Option<Value>,

    /// The object as the engine stored it.
    pub after: Value,
}

impl WriteOutcome {
    /// Whether the write changed nothing.
    pub fn is_noop(&self) -> bool {
        !self.created && self.changed.is_empty()
    }
}

/// Applies an RFC 7396 JSON Merge Patch to `target`.
///
/// Implemented **once**, here, because every write tool needs exactly this and a second
/// implementation would differ in the `null` case. The rule, in full:
///
/// - a patch that is not an object **replaces** the target outright;
/// - `null` **deletes** the member;
/// - an object member recurses;
/// - anything else, arrays included, **replaces** wholesale.
///
/// Arrays replacing wholesale is the part worth knowing: there is no way to append to a list
/// with a merge patch, which is why `apply_item` describes it to the model as "send only what
/// you want to change".
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Some(patch) = patch.as_object() else {
        *target = patch.clone();
        return;
    };

    if !target.is_object() {
        *target = Value::Object(Map::new());
    }

    // PANIC-free: `target` was just made an object if it was not one.
    let Some(target) = target.as_object_mut() else {
        return;
    };

    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
            continue;
        }

        match target.get_mut(key) {
            Some(existing) => merge_patch(existing, value),
            None => {
                let mut fresh = Value::Null;
                merge_patch(&mut fresh, value);
                target.insert(key.clone(), fresh);
            }
        }
    }
}

/// Removes the fields a `PUT` would ignore or refuse (§8.5).
///
/// Leaving `customFields` in would be worse than a wasted field: the engine ignores it and
/// warns, so the model would be told a write happened that did not.
pub fn strip_server_owned(manifest: &mut Value) {
    let Some(object) = manifest.as_object_mut() else {
        return;
    };

    for field in SERVER_OWNED_FIELDS {
        object.remove(*field);
    }

    if let Some(Value::Object(metadata)) = object.get_mut("metadata") {
        for field in READ_ONLY_METADATA_FIELDS {
            metadata.remove(*field);
        }
    }
}

/// The field paths at which two manifests differ, in document order.
///
/// Used to compute [`WriteOutcome::changed`] by comparing the pre-read against the result, so a
/// tool can say "nothing changed" and mean it.
pub fn changed_paths(before: &Value, after: &Value) -> Vec<FieldPath> {
    let mut paths = Vec::new();
    collect_changes("", before, after, &mut paths);

    paths
}

/// Walks two values in parallel, recording where they diverge.
fn collect_changes(prefix: &str, before: &Value, after: &Value, paths: &mut Vec<FieldPath>) {
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            // Sorted here, not by the map — which keeps insertion order under `preserve_order` —
            // so the report is stable whatever order either manifest's keys arrived in.
            let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
            keys.sort_unstable();
            keys.dedup();

            for key in keys {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };

                match (before.get(key), after.get(key)) {
                    (Some(before), Some(after)) => collect_changes(&path, before, after, paths),
                    (None, Some(_)) | (Some(_), None) => paths.push(path),
                    (None, None) => {}
                }
            }
        }
        // Arrays are compared whole: a merge patch replaces them whole, so reporting an element
        // index would describe an operation the caller cannot express.
        _ if before != after => paths.push(prefix.to_string()),
        _ => {}
    }
}

/// The one read-merge-write helper (P8, P7, P10, D29).
///
/// `apply` does: read (a `404` means create) → merge the patch → strip the fields a `PUT`
/// ignores → write with the `resourceVersion` in the place this endpoint wants it → on a `409`,
/// either re-read and re-apply **once** or report, whichever the policy says.
///
/// Implemented once so that six tools do not each re-derive the conflict rule, the strip list,
/// or which endpoint takes `resourceVersion` where.
pub struct WriteCycle<'a> {
    engine: &'a EngineClient,
    policy: ConflictPolicy,
    resource_version_in: ResourceVersionIn,
}

impl<'a> WriteCycle<'a> {
    /// Builds a cycle for one endpoint's conventions.
    pub fn new(
        engine: &'a EngineClient,
        policy: ConflictPolicy,
        resource_version_in: ResourceVersionIn,
    ) -> Self {
        Self {
            engine,
            policy,
            resource_version_in,
        }
    }

    /// Reads, merges, writes an **item** — and reports honestly what changed.
    ///
    /// # Errors
    ///
    /// Every failure is already the contract's shape. A `409` the policy declines to retry is a
    /// `conflict`; a write that failed after leaving is `unknown_outcome` (D20).
    pub async fn apply(
        &self,
        address: &ItemAddress,
        patch: &Value,
    ) -> Result<WriteOutcome, ToolError> {
        self.run(address, patch, Existence::Any).await
    }

    /// The same cycle for an **Item Type Definition** (T12), read and written raw (DR-86), with the
    /// caller's expectation of whether it exists checked before anything is sent.
    ///
    /// # Errors
    ///
    /// Those of [`Self::apply`]; and, before any write, the read's `not_found` under
    /// [`Existence::Present`], or a `conflict` under [`Existence::Absent`] when the definition
    /// already exists.
    pub async fn apply_item_type(
        &self,
        address: &ItemTypeAddress,
        patch: &Value,
        existence: Existence,
    ) -> Result<WriteOutcome, ToolError> {
        self.run(address, patch, existence).await
    }

    /// The cycle itself, for any resource.
    async fn run<R: Resource>(
        &self,
        resource: &R,
        patch: &Value,
        existence: Existence,
    ) -> Result<WriteOutcome, ToolError> {
        let (before, created) = match resource.read(self.engine).await {
            Ok(_) if existence == Existence::Absent => {
                return Err(ToolError::new(
                    codes::CONFLICT,
                    Remedy::RetryAfterChange,
                    format!("`{resource}` already exists, so nothing was created."),
                ));
            }
            Ok(current) => (current, false),
            Err(error) if error.code == codes::NOT_FOUND && existence != Existence::Present => {
                (Value::Object(Map::new()), true)
            }
            Err(error) => return Err(error),
        };

        let outcome = self.write_once(resource, &before, patch, created).await;

        match outcome {
            Err(error)
                if error.code == codes::CONFLICT && self.policy == ConflictPolicy::RetryOnce =>
            {
                // Re-read and re-apply **once**: a merge-patch intent means the same thing
                // whatever else moved underneath, which is exactly when a retry is honest.
                tracing::info!(%resource, "re-applying a merge patch after a conflict");

                let before = resource.read(self.engine).await?;

                let mut outcome = self.write_once(resource, &before, patch, false).await?;
                outcome.retried = true;

                Ok(outcome)
            }
            other => other,
        }
    }

    /// One read-merge-write pass, with no conflict handling of its own.
    async fn write_once<R: Resource>(
        &self,
        resource: &R,
        before: &Value,
        patch: &Value,
        created: bool,
    ) -> Result<WriteOutcome, ToolError> {
        let mut manifest = before.clone();
        merge_patch(&mut manifest, patch);

        let resource_version = before
            .get("resourceVersion")
            .and_then(Value::as_str)
            .map(str::to_string);

        strip_server_owned(&mut manifest);
        self.place_resource_version(&mut manifest, resource_version.as_deref());

        let written = resource
            .write(
                self.engine,
                &manifest,
                self.policy == ConflictPolicy::RetryOnce,
            )
            .await?;

        Ok(WriteOutcome {
            created,
            changed: changed_paths(&comparable(before), &comparable(&written.value)),
            retried: false,
            warnings: written.warnings,
            before: (!created).then(|| before.clone()),
            after: written.value,
        })
    }

    /// Puts the `resourceVersion` where this endpoint expects it (§8.5).
    ///
    /// The query-parameter placement is applied by the operation rather than the body, so here
    /// it means only *"leave it out of the manifest"*.
    fn place_resource_version(&self, manifest: &mut Value, resource_version: Option<&str>) {
        let Some(object) = manifest.as_object_mut() else {
            return;
        };

        match (self.resource_version_in, resource_version) {
            (ResourceVersionIn::Body, Some(version)) => {
                object.insert("resourceVersion".to_string(), Value::String(version.into()));
            }
            (ResourceVersionIn::Body, None) | (ResourceVersionIn::Query, _) => {
                object.remove("resourceVersion");
            }
        }
    }
}

/// Something the write cycle can read and write whole: an item, or an Item Type Definition.
///
/// Private: the cycle is the only caller, and the two implementations are the only resources a
/// `PUT` replaces in this tool set.
trait Resource: std::fmt::Display + Sync {
    /// The current state, as JSON.
    fn read(&self, engine: &EngineClient) -> impl Future<Output = Result<Value, ToolError>> + Send;

    /// Writes `manifest` whole, returning what the engine stored.
    fn write(
        &self,
        engine: &EngineClient,
        manifest: &Value,
        retryable: bool,
    ) -> impl Future<Output = Result<EngineResponse<Value>, ToolError>> + Send;
}

/// An item is read through the typed model, which carries every column a `PUT` writes.
impl Resource for ItemAddress {
    async fn read(&self, engine: &EngineClient) -> Result<Value, ToolError> {
        let response = engine.get_item(self).await?;

        serde_json::to_value(&response.value).map_err(unserialisable)
    }

    async fn write(
        &self,
        engine: &EngineClient,
        manifest: &Value,
        retryable: bool,
    ) -> Result<EngineResponse<Value>, ToolError> {
        let written = engine.put_item(self, manifest, retryable).await?;

        Ok(EngineResponse {
            value: serde_json::to_value(&written.value).map_err(unserialisable)?,
            warnings: written.warnings,
        })
    }
}

/// A definition is read and written **raw**: the typed model does not declare every field a
/// version may carry, and `spec.versions` is replaced whole (DR-86).
impl Resource for ItemTypeAddress {
    async fn read(&self, engine: &EngineClient) -> Result<Value, ToolError> {
        Ok(engine.get_item_type_definition(self).await?.value)
    }

    async fn write(
        &self,
        engine: &EngineClient,
        manifest: &Value,
        retryable: bool,
    ) -> Result<EngineResponse<Value>, ToolError> {
        engine
            .put_item_type_definition(self, manifest, retryable)
            .await
    }
}

/// A manifest as `changed` compares it: without the fields the server rewrites on every write.
///
/// The engine's `UPDATE` runs even when nothing differs, so `resourceVersion` (Postgres `xmin`)
/// and `metadata.updateTimestamp` move on **every** `PUT`. Diffing them would make a no-op write
/// indistinguishable from a real one, which is the one thing `changed` exists to tell apart
/// (T8-D7). What is stripped is exactly what a write cannot set, so nothing the caller could have
/// changed is hidden.
fn comparable(manifest: &Value) -> Value {
    let mut manifest = manifest.clone();
    strip_server_owned(&mut manifest);

    manifest
}

/// A manifest we built and cannot serialise is a defect of ours.
fn unserialisable(err: serde_json::Error) -> ToolError {
    tracing::error!(?err, "a manifest this server built is not serialisable");

    ToolError::new(
        codes::SERVER_DEFECT,
        Remedy::Escalate,
        "This server built a write it could not send.",
    )
}

#[cfg(test)]
mod tests;
