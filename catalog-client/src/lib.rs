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
// The typed client for `catalog-engine` (§8).
//
// **One typed client, one place** (D24). It owns every URL, query parameter, header, projection
// and status-code mapping, so no tool builds a URL or reads a header. It is a separate crate
// because it is the half with a second consumer already visible — `ai-foundry-bff` re-describes
// Catalog models today — and because it is the half whose correctness is domain-critical and
// worth testing without a server in the way.

/// Where an item lives, validated against the engine's own regexes (§8.1).
pub mod address;

/// The HTTP client, its timeouts, the retry policy and the deadline (§8.1).
pub mod client;

/// The tool-error contract: `Remedy`, `ToolError` and the status mapping (§8.4, D19).
pub mod error;

/// The caller's forwarded identity (§7.4, D26, D45).
pub mod identity;

/// The engine's wire models.
pub mod models;

/// Pages, engine cursors and the opaque cursors we mint (§8.2, D32).
pub mod pagination;

/// `Accept` projections, and the grouping that makes an invalid pair unconstructible (§8.2).
pub mod projection;

/// The `query → rawq` translator: AST, four operators, limits, splitting (§8.8, D33).
pub mod query;

/// `kind → {group, version, family}`, and the served-version rule (§8.6, P9, D30).
pub mod resolve;

/// One `Warning: 299 - "…"` parser, for every response (§8.3, P6, D28).
pub mod warning;

/// The one read-merge-write helper: RFC 7396, the conflict rule, the diff (§8.5, D23, D29).
pub mod write;

/// One module-level function per engine operation the tools use (§8.1).
pub mod ops;

/// The mock engine and its fixture library (§12.5). Behind the `testing` feature.
#[cfg(feature = "testing")]
pub mod testing;

pub use address::ItemAddress;
pub use client::{CallWarnings, Deadline, EngineClient, EngineClientFactory, EngineResponse};
pub use error::{Remedy, ToolError};
pub use identity::{AclContext, CallerIdentity, Sensitive, TenantKey};
pub use models::Tenant;
pub use pagination::{EngineCursor, ListPage, ToolCursor};
pub use projection::{Grouping, Projection};
pub use query::{FieldPath, Predicate, QueryValue, RegexLiteral};
pub use resolve::{ServedVersion, TypeCoordinates, resolve_kind, select_served_version};
pub use warning::EngineWarning;
pub use write::{ConflictPolicy, ResourceVersionIn, WriteCycle, WriteOutcome, merge_patch};
