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
// The closed error-code set of §8.4, in one place.
//
// D19 says one shape for every tool, which is only true if every code is listed somewhere a test
// can walk. `ALL_CODES` is that list, and `tests::test_code_set_matches_the_documented_tables`
// asserts it equals the union of §8.4's two tables — so adding an error without adding a row
// fails CI.

// ---------------------------------------------------------------------------------------------
// §8.4, first table: raised from an engine outcome.
// ---------------------------------------------------------------------------------------------

/// A `400` the caller can fix, or a limit we validated before dialling. The two share a code
/// deliberately: same fix, same words, whether we or the engine caught it.
pub const INVALID_INPUT: &str = "invalid_input";

/// A `400`, `406` or `415` on something **we** built. The model is told it is not its fault.
pub const SERVER_DEFECT: &str = "server_defect";

/// A `401`. Phrased as identity not reaching the service, never as a catalog problem.
pub const UNAUTHENTICATED: &str = "unauthenticated";

/// A `403`. The model cannot widen its own scope (NFR-01).
pub const FORBIDDEN: &str = "forbidden";

/// A `404` on an item or a type.
pub const NOT_FOUND: &str = "not_found";

/// A `409`, after the write helper's own conflict policy has decided whether to retry (D23).
pub const CONFLICT: &str = "conflict";

/// A `422`: this kind cannot have versions, or custom fields, or whatever was asked of it.
pub const UNSUPPORTED_FOR_TYPE: &str = "unsupported_for_type";

/// A declared `501`.
pub const NOT_IMPLEMENTED: &str = "not_implemented";

/// A `502` from an authorization or identity upstream. Says **authz**, not *the catalog*.
pub const UPSTREAM_UNAVAILABLE: &str = "upstream_unavailable";

/// A `5XX` or a transport failure on a read: *"unavailable, not empty"*.
pub const CATALOG_UNAVAILABLE: &str = "catalog_unavailable";

/// A `5XX`, transport failure or deadline **after** a write or delete was dispatched (D20).
pub const UNKNOWN_OUTCOME: &str = "unknown_outcome";

/// The deadline ran out on a read.
pub const DEADLINE_EXCEEDED: &str = "deadline_exceeded";

// ---------------------------------------------------------------------------------------------
// §8.4, second table: raised by the runtime and the client, with no engine status behind them.
// ---------------------------------------------------------------------------------------------

/// Tool arguments did not deserialise; the serde path goes in `details.field`.
pub const INVALID_ARGUMENTS: &str = "invalid_arguments";

/// A cursor that does not decode, carries the wrong version, or whose filter fingerprint does
/// not match. **Never** silently treated as end-of-results (D32).
pub const INVALID_CURSOR: &str = "invalid_cursor";

/// `metadata.family == null` on a resolved item — a real engine state, not a lookup miss (D30).
pub const UNADDRESSABLE_ITEM: &str = "unaddressable_item";

/// The kind resolves but has no `served: true` version (§8.6).
pub const UNADDRESSABLE_TYPE: &str = "unaddressable_type";

/// The query exceeds the parameter-count or byte split budget (D33).
pub const QUERY_TOO_LARGE: &str = "query_too_large";

/// The client disconnected mid-call (§5.5 rule 5).
pub const CANCELLED: &str = "cancelled";

/// The per-tenant call rate was exceeded (§6.4). A tool error, not a `429`, so the model can
/// read it and wait.
pub const RATE_LIMITED: &str = "rate_limited";

/// Every code in the closed set, in the order §8.4's tables list them.
pub const ALL_CODES: &[&str] = &[
    INVALID_INPUT,
    SERVER_DEFECT,
    UNAUTHENTICATED,
    FORBIDDEN,
    NOT_FOUND,
    CONFLICT,
    UNSUPPORTED_FOR_TYPE,
    NOT_IMPLEMENTED,
    UPSTREAM_UNAVAILABLE,
    CATALOG_UNAVAILABLE,
    UNKNOWN_OUTCOME,
    DEADLINE_EXCEEDED,
    INVALID_ARGUMENTS,
    INVALID_CURSOR,
    UNADDRESSABLE_ITEM,
    UNADDRESSABLE_TYPE,
    QUERY_TOO_LARGE,
    CANCELLED,
    RATE_LIMITED,
];
