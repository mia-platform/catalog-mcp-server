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
// The typed client for `catalog-engine`.
//
// **One typed client, one place**. It owns every URL, query parameter, header, projection
// and status-code mapping, so no tool builds a URL or reads a header. It is a separate crate
// because it is the half with a second consumer already visible — `ai-foundry-bff` re-describes
// Catalog models today — and because it is the half whose correctness is domain-critical and
// worth testing without a server in the way.

/// Where an item lives, validated against the engine's own regexes.
pub mod address;

/// The HTTP client, its timeouts, the retry policy and the deadline.
pub mod client;

/// The tool-error contract: `Remedy`, `ToolError` and the status mapping.
pub mod error;

/// The caller's forwarded identity.
pub mod identity;

/// The engine's wire models.
pub mod models;

/// Pages, engine cursors and the opaque cursors we mint.
pub mod pagination;

/// `Accept` projections, and the grouping that makes an invalid pair unconstructible.
pub mod projection;

/// The `query → rawq` translator: AST, four operators, limits, splitting.
pub mod query;

/// `kind → {group, version, family}`, and the served-version rule.
pub mod resolve;

/// One `Warning: 299 - "…"` parser, for every response.
pub mod warning;

/// The one read-merge-write helper: RFC 7396, the conflict rule, the diff.
pub mod write;

/// One module-level function per engine operation the tools use.
pub mod ops;

/// The mock engine and its fixture library. Behind the `testing` feature.
#[cfg(feature = "testing")]
pub mod testing;

pub use address::{FamilyAddress, ItemAddress, ItemTypeAddress, is_valid_name};
pub use client::{
    BAGGAGE_HEADER, CallWarnings, Deadline, EngineClient, EngineClientFactory, EngineResponse,
    TRACEPARENT_HEADER, TRACESTATE_HEADER, TraceContext,
};
pub use error::{Remedy, ToolError};
pub use identity::{AclContext, CallerIdentity, Sensitive, TenantKey};
pub use pagination::{EngineCursor, ListPage, ToolCursor};
pub use projection::{Grouping, Projection};
pub use query::{FieldPath, Predicate, QueryValue, RegexLiteral};
pub use resolve::{
    ItemTypeDocument, KindResolution, ServedVersion, TypeCoordinates, coordinates_of,
    find_item_type, find_item_type_document, find_item_type_document_if_any,
    find_item_type_document_or_suggest, find_item_type_or_suggest, is_valid_kind, resolve_kind,
    resolve_kind_or_shared, resolve_kind_or_suggest, select_served_version,
};
pub use warning::EngineWarning;
pub use write::{
    ConflictPolicy, Existence, ResourceVersionIn, WriteCycle, WriteOutcome, merge_patch,
};
