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
// T2 §6 — what a search cursor pins, and the fingerprint that stops it being replayed against a
// different search. The wrapper, its versioning and its encoding are the core's `ToolCursor`.

use catalog_client::{
    FamilyAddress, Predicate, Remedy, ToolError,
    error::codes,
    pagination::{EngineCursor, ToolCursor, fingerprint as fingerprint_of},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// What the next page of a search needs that its arguments do not carry.
#[derive(Serialize, Deserialize)]
struct SearchPinned {
    /// The family the search was resolved to, so a later page does not resolve `kind` again.
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    coordinates: Option<PinnedFamily>,

    /// How many items earlier pages returned — what makes a later page's `total` right without
    /// a count call when that page is not full (T2-D5).
    #[serde(rename = "n")]
    returned: u64,
}

/// A family's coordinates as a cursor carries them — re-validated on the way back in.
#[derive(Serialize, Deserialize)]
struct PinnedFamily {
    #[serde(rename = "g")]
    group: String,

    #[serde(rename = "v")]
    version: String,

    #[serde(rename = "f")]
    family: String,
}

/// Where a resumed search picks up.
pub struct Resumed {
    /// The engine's continuation token.
    pub engine: EngineCursor,

    /// The family, when the search was restricted to a `kind`.
    pub family: Option<FamilyAddress>,

    /// How many items earlier pages returned.
    pub returned: u64,
}

/// The fingerprint of one search: its predicate and the `kind` it was restricted to.
///
/// The `kind` **argument** stands in for the resolved coordinates the plan names: the
/// coordinates are only known after the cursor is decoded, and the same `kind` resolves to the
/// same family, so pinning the argument gives the same protection. Changing any filter, the
/// query or the `kind` makes an old cursor refuse to continue.
pub fn fingerprint(kind: Option<&str>, predicate: Option<&Predicate>) -> String {
    fingerprint_of(&json!({
        "kind": kind,
        "query": predicate.map(Predicate::to_json),
    }))
}

/// Mints the cursor for the page after this one.
pub fn mint(
    next: &EngineCursor,
    fingerprint: &str,
    family: Option<&FamilyAddress>,
    returned: u64,
) -> Result<String, ToolError> {
    ToolCursor::new(
        Some(next),
        fingerprint,
        SearchPinned {
            coordinates: family.map(|family| PinnedFamily {
                group: family.group().to_string(),
                version: family.version().to_string(),
                family: family.family().to_string(),
            }),
            returned,
        },
    )
    .encode()
}

/// Decodes a cursor handed back, refusing one that belongs to another search or does not hold
/// together.
///
/// **Never** treated as end-of-results: a model that read a bad cursor as "no more" would
/// silently truncate its own answer (D32).
pub fn resume(
    raw: &str,
    fingerprint: &str,
    restricted_to_kind: bool,
) -> Result<Resumed, ToolError> {
    let cursor = ToolCursor::<SearchPinned>::decode(raw, fingerprint)?;
    let engine = cursor.engine_cursor().ok_or_else(unusable_cursor)?;

    let family = match (cursor.pinned.coordinates, restricted_to_kind) {
        (Some(pinned), true) => Some(
            FamilyAddress::new(pinned.group, pinned.version, pinned.family)
                .map_err(|_| unusable_cursor())?,
        ),
        (None, false) => None,
        // The fingerprint pins `kind`, so this only happens to a cursor that was altered.
        _ => return Err(unusable_cursor()),
    };

    Ok(Resumed {
        engine,
        family,
        returned: cursor.pinned.returned,
    })
}

/// A cursor that decoded but cannot be continued from.
fn unusable_cursor() -> ToolError {
    ToolError::new(
        codes::INVALID_CURSOR,
        Remedy::RetryAfterChange,
        "That cursor cannot be continued from.",
    )
    .with_next_step("start the listing again without a cursor")
}
