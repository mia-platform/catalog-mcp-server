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
// The audit log's own shapes: its page envelope, which is not the catalog's `List`, and an entry,
// with only the fields this server reads. Anything else the engine adds is ignored on the way in.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One page of `GET /audit-log`: offset-addressed, and — unlike every other listing — with a total.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditLogPage {
    /// Where this page sits, and how many entries match.
    #[serde(rename = "metadata")]
    pub metadata: AuditLogPageMetadata,

    /// The entries, newest first.
    #[serde(rename = "items", default)]
    pub items: Vec<AuditLogEntry>,
}

/// The audit page's position and count.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditLogPageMetadata {
    /// How many matching entries come before this page.
    #[serde(rename = "skip")]
    pub skip: u64,

    /// The page size asked for.
    #[serde(rename = "limit")]
    pub limit: u64,

    /// How many entries match — exact up to the engine's count cap, the cap itself beyond it.
    #[serde(rename = "total")]
    pub total: u64,

    /// Whether `total` is the real count rather than the cap.
    #[serde(rename = "totalIsExact")]
    pub total_is_exact: bool,
}

/// One recorded call.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditLogEntry {
    /// The entry's own id.
    #[serde(rename = "uid")]
    pub uid: String,

    /// When the call started, to the second.
    #[serde(rename = "timestamp")]
    pub timestamp: String,

    /// What was done.
    #[serde(rename = "action")]
    pub action: AuditAction,

    /// Who did it; absent for a call with no attributable identity.
    #[serde(rename = "caller", default)]
    pub caller: Option<AuditCaller>,

    /// What it was done to.
    #[serde(rename = "resource")]
    pub resource: AuditResource,

    /// How it ended.
    #[serde(rename = "result")]
    pub result: AuditResult,

    /// A few action-specific facts — never a request payload.
    #[serde(rename = "details", default)]
    pub details: Option<Value>,
}

/// An action, as an entry carries it and as the vocabulary lists it.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditAction {
    /// The compact code the `action` filter takes.
    #[serde(rename = "code")]
    pub code: String,

    /// The sentence-cased description.
    #[serde(rename = "description", default)]
    pub description: String,

    /// The short, title-cased label.
    #[serde(rename = "displayName", default)]
    pub display_name: String,
}

/// The identity a call was attributed to.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditCaller {
    /// The principal's id — what the `principal` filter takes.
    #[serde(rename = "id")]
    pub id: String,

    /// The principal's name, when it resolved.
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
}

/// The resource an entry names, resolved against the current catalog.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditResource {
    /// `instance`, `collection` or `none`.
    #[serde(rename = "scope")]
    pub scope: String,

    /// `Item`, `ItemTypeDefinition`, `TenantSettings` or `Webhook`.
    #[serde(rename = "kind", default)]
    pub kind: Option<String>,

    /// The resource's URN.
    #[serde(rename = "urn", default)]
    pub urn: Option<String>,

    /// The human-readable name of the resource's type.
    #[serde(rename = "typeDisplayName", default)]
    pub type_display_name: Option<String>,

    /// The resource's current title, else the one it had, else its name.
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,

    /// Whether the resource can be found now.
    #[serde(rename = "exists", default = "default_exists")]
    pub exists: bool,
}

/// An entry without `exists` names nothing that could have gone missing.
fn default_exists() -> bool {
    true
}

/// How a recorded call ended.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct AuditResult {
    /// `success` or `error`.
    #[serde(rename = "outcome")]
    pub outcome: String,

    /// The HTTP status the caller received.
    #[serde(rename = "status")]
    pub status: u16,

    /// The refused call's own error body, or a truncation marker when it was too large to keep.
    #[serde(rename = "error", default)]
    pub error: Option<Value>,
}
