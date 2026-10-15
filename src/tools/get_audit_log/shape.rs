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
// An audit entry as a model reads it: who, what, when and how it ended, plus the caller's id it
// can filter on next and the few facts the engine attached. The request's forensics stay out.

use catalog_client::models::AuditLogEntry;
use serde::Serialize;
use serde_json::Value;

/// The URN scheme every catalog URN starts with: `urn:mia-platform-catalog:<group>:<version>:…`.
const URN_PREFIX: &str = "urn:mia-platform-catalog:";

/// What an entry with no caller is attributed to — a background job, or a route no policy guards.
pub const SYSTEM_CALLER: &str = "system";

/// What a tenant-settings entry names, since the engine gives it no name of its own.
const TENANT_SETTINGS_NAME: &str = "tenant settings";

/// The resource kinds the engine records that are not items.
const ITEM_KIND: &str = "Item";
const WEBHOOK_KIND: &str = "Webhook";
const TENANT_SETTINGS_KIND: &str = "TenantSettings";

/// The scopes that need their own naming.
const COLLECTION_SCOPE: &str = "collection";
const NO_SCOPE: &str = "none";

/// The outcome that may carry an error.
const ERROR_OUTCOME: &str = "error";

/// One entry, shaped.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct ShapedEntry {
    /// When the call started, to the second.
    #[serde(rename = "time")]
    pub time: String,

    /// The action's code, which is what the `action` filter takes.
    #[serde(rename = "action")]
    pub action: String,

    /// The caller's name; its id when the name did not resolve; `system` when there is no caller.
    #[serde(rename = "who")]
    pub who: String,

    /// The caller's id, which is what the `principal` filter takes. Absent for `system`.
    #[serde(rename = "whoId", skip_serializing_if = "Option::is_none")]
    pub who_id: Option<String>,

    /// What the call was about: the resource's current name, or what stands for it.
    #[serde(rename = "what", skip_serializing_if = "Option::is_none")]
    pub what: Option<String>,

    /// An item's kind, or the kind of resource.
    #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    /// `success` or `error`.
    #[serde(rename = "outcome")]
    pub outcome: String,

    /// The HTTP status the caller received.
    #[serde(rename = "status")]
    pub status: u16,

    /// Why a refused call was refused, when the engine kept its error.
    #[serde(rename = "error", skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// `false` when the resource does not exist now; absent otherwise.
    #[serde(rename = "exists", skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,

    /// The facts the engine attached to this action, as it recorded them.
    #[serde(rename = "details", skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Shapes one entry. Nothing is dropped for a missing field: each falls back to what the entry
/// does say.
pub fn shape(entry: AuditLogEntry) -> ShapedEntry {
    let what = what_of(&entry);
    let kind = kind_of(&entry);
    let exists = (entry.resource.scope != NO_SCOPE && !entry.resource.exists).then_some(false);
    let error = (entry.result.outcome == ERROR_OUTCOME)
        .then(|| {
            entry
                .result
                .error
                .as_ref()
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();

    let (who, who_id) = match entry.caller {
        Some(caller) => (
            caller
                .display_name
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| caller.id.clone()),
            Some(caller.id),
        ),
        None => (SYSTEM_CALLER.to_string(), None),
    };

    ShapedEntry {
        time: entry.timestamp,
        action: entry.action.code,
        who,
        who_id,
        what,
        kind,
        outcome: entry.result.outcome,
        status: entry.result.status,
        error,
        exists,
        details: entry.details,
    }
}

/// What the entry names, falling back where the engine has no name: a webhook by its type, tenant
/// settings as such, a whole-type entry by the type, a tenant-wide one by the action itself.
fn what_of(entry: &AuditLogEntry) -> Option<String> {
    let resource = &entry.resource;

    if let Some(name) = resource
        .display_name
        .as_ref()
        .filter(|name| !name.is_empty())
    {
        return Some(name.clone());
    }

    match resource.kind.as_deref() {
        Some(WEBHOOK_KIND) => {
            return Some(match &resource.type_display_name {
                Some(type_name) => format!("webhook of {type_name}"),
                None => "webhook".to_string(),
            });
        }
        Some(TENANT_SETTINGS_KIND) => return Some(TENANT_SETTINGS_NAME.to_string()),
        _ => {}
    }

    match resource.scope.as_str() {
        COLLECTION_SCOPE => resource.type_display_name.clone(),
        NO_SCOPE => Some(entry.action.description.clone()).filter(|text| !text.is_empty()),
        _ => resource
            .urn
            .as_deref()
            .and_then(|urn| urn.rsplit(':').next())
            .map(str::to_string),
    }
}

/// An item's own kind, read from its URN; any other resource's kind as the engine names it.
fn kind_of(entry: &AuditLogEntry) -> Option<String> {
    let resource = &entry.resource;

    match resource.kind.as_deref() {
        Some(ITEM_KIND) => resource
            .urn
            .as_deref()
            .and_then(item_kind_of)
            .or_else(|| resource.kind.clone()),
        _ => resource.kind.clone(),
    }
}

/// `urn:mia-platform-catalog:<group>:<version>:<kind>:<name>` → `<kind>`.
fn item_kind_of(urn: &str) -> Option<String> {
    let parts: Vec<&str> = urn.strip_prefix(URN_PREFIX)?.split(':').collect();

    match parts.as_slice() {
        [_, _, kind, _] if !kind.is_empty() => Some((*kind).to_string()),
        _ => None,
    }
}
