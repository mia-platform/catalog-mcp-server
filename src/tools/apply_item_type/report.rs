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
// What a type write did beyond `changed`: the fields it did not apply, whether the items'
// schema moved, which served versions went away, and the background work it started.

use catalog_client::EngineWarning;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Why a read-only field was not applied — the reason the model relays.
pub(super) const READ_ONLY_REASON: &str = "read-only after creation";

/// Why `spec.history` or `spec.audit` was not applied: not merely read-only, but out of
/// reach of every tool in this set.
pub(super) const CREATE_ONLY_REASON: &str =
    "set only when the type is created; this tool cannot change it";

/// Why a field that names the type was not applied: `spec.group`, `spec.names.plural` and
/// `metadata.name` are its address.
pub(super) const IDENTITY_REASON: &str = "identifies the type; set only when it is created";

/// The two settings the engine applies on creation only.
const CREATE_ONLY_FIELDS: [&str; 2] = ["spec.history", "spec.audit"];

/// The background job every history-enabled creation starts.
pub(super) const REVISION_BACKFILL_JOB: &str = "revision backfill";

/// The job a history-enabled creation with a numeric retention starts: it deletes revisions.
pub(super) const RETENTION_TRIM_JOB: &str = "retention trim (may delete revisions)";

/// The retention policy that keeps everything, and so starts no trim.
const RETAIN_ALL_POLICY: &str = "All";

/// One field the write did not apply, and why.
#[derive(Clone, Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct Ignored {
    #[serde(rename = "field")]
    pub(super) field: String,

    #[serde(rename = "reason")]
    pub(super) reason: &'static str,
}

impl Ignored {
    /// An ignored `field`, with the reason its kind of field carries.
    pub(super) fn read_only(field: &str) -> Self {
        let reason = if CREATE_ONLY_FIELDS.contains(&field) {
            CREATE_ONLY_REASON
        } else {
            READ_ONLY_REASON
        };

        Self {
            field: field.to_string(),
            reason,
        }
    }
}

/// `ignored`, from what this tool held back and what the engine reported ignoring — in that order,
/// each field once.
///
/// The engine's side comes from its `Warning: 299` headers through the core's named parser
/// (`EngineWarning::read_only_field`); the headers themselves stay in `warnings`, verbatim, so a
/// change in their wording is diagnosable rather than silent.
pub(super) fn ignored(held_back: Vec<Ignored>, warnings: &[EngineWarning]) -> Vec<Ignored> {
    let mut ignored = held_back;

    for field in warnings.iter().filter_map(EngineWarning::read_only_field) {
        if !ignored.iter().any(|entry| entry.field == field) {
            ignored.push(Ignored::read_only(field));
        }
    }

    ignored
}

/// Each version's `openAPIV31Schema`, by version name.
fn schemas(definition: &Value) -> BTreeMap<&str, Option<&Value>> {
    versions(definition)
        .filter_map(|version| {
            let name = version.get("name")?.as_str()?;

            Some((
                name,
                version
                    .get("schema")
                    .and_then(|schema| schema.get("openAPIV31Schema")),
            ))
        })
        .collect()
}

/// Every entry of `spec.versions`.
fn versions(definition: &Value) -> impl Iterator<Item = &Value> {
    definition
        .get("spec")
        .and_then(|spec| spec.get("versions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The names of the versions that are `served: true`, in document order.
pub(super) fn served_versions(definition: &Value) -> Vec<String> {
    versions(definition)
        .filter(|version| version.get("served").and_then(Value::as_bool) == Some(true))
        .filter_map(|version| version.get("name")?.as_str().map(str::to_string))
        .collect()
}

/// Every version name the definition declares, served or not.
pub(super) fn version_names(definition: &Value) -> Vec<String> {
    versions(definition)
        .filter_map(|version| version.get("name")?.as_str().map(str::to_string))
        .collect()
}

/// Whether the schema items are validated against moved: a version's schema changed, or
/// a version was added or taken away.
pub(super) fn schema_changed(before: &Value, after: &Value) -> bool {
    schemas(before) != schemas(after)
}

/// The versions served before the write and not after it, whose items have lost their path.
pub(super) fn versions_removed(before: &Value, after: &Value) -> Vec<String> {
    let still_served = served_versions(after);

    served_versions(before)
        .into_iter()
        .filter(|version| !still_served.contains(version))
        .collect()
}

/// The asynchronous jobs a **creation** starts, named so a model does not report a settled
/// state.
///
/// Empty on an update. The engine enqueues the same two jobs after every write of a history-enabled
/// type, but on an update they are re-runs of work already done: `spec.history` cannot change
/// through this tool, so nothing new is backfilled or trimmed.
pub(super) fn background_jobs(created: bool, after: &Value) -> Vec<&'static str> {
    let history = after.get("spec").and_then(|spec| spec.get("history"));
    let enabled = history
        .and_then(|history| history.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);

    if !created || !enabled {
        return Vec::new();
    }

    let keeps_everything = history
        .and_then(|history| history.get("retention"))
        .and_then(|retention| retention.get("policy"))
        .and_then(Value::as_str)
        == Some(RETAIN_ALL_POLICY);

    let mut jobs = vec![REVISION_BACKFILL_JOB];
    if !keeps_everything {
        jobs.push(RETENTION_TRIM_JOB);
    }

    jobs
}
