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
    error::{Remedy, ToolError, codes},
    models::ListEnvelope,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Serialize, de::DeserializeOwned};

/// The engine's own maximum `limit`; anything above it is a `400`.
pub const MAX_LIMIT: u32 = 200;

/// The engine's own default `limit`, which the tools take rather than inventing one.
pub const DEFAULT_LIMIT: u32 = 50;

/// How many pages an internal walk may fetch before giving up (§8.2).
///
/// It bounds a runaway **loop**, not a payload: twenty pages of two hundred is up to four
/// thousand items, each of unbounded size, and nothing counts bytes on the way in. That is a
/// recorded, accepted risk for v1 (§9, §17.3), not an oversight.
pub const MAX_INTERNAL_PAGES: usize = 20;

/// The version of the cursor format we mint. A cursor carrying anything else is refused rather
/// than guessed at.
const CURSOR_VERSION: u8 = 1;

/// The engine's opaque continuation token. Never handed to the model (D32).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineCursor(String);

impl EngineCursor {
    /// Wraps a token the engine minted.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token, for the `continue` query parameter.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One page of a listing.
#[derive(Clone, Debug, PartialEq)]
pub struct ListPage<T> {
    /// This page's objects.
    pub items: Vec<T>,

    /// Where the next page starts, or `None` at the end of the listing.
    pub next: Option<EngineCursor>,
}

impl<T> ListPage<T> {
    /// Unwraps the engine's `{metadata: {continue}}` envelope.
    pub fn from_envelope(envelope: ListEnvelope<T>) -> Self {
        Self {
            items: envelope.items,
            next: envelope.metadata.continue_token.map(EngineCursor::new),
        }
    }
}

/// The cursor a tool hands the model: **ours, never the engine's** (D32).
///
/// The wrapper carries the engine token plus whatever the tool must pin — the audit window's
/// `until`, the projection, the filters — and a fingerprint of the query it was minted against,
/// so a cursor cannot be replayed against a different one.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct ToolCursor<P> {
    /// The cursor format version.
    #[serde(rename = "v")]
    pub version: u8,

    /// The engine's continuation token, when the tool paginates through the engine. `None` for
    /// a tool whose pagination is entirely its own.
    #[serde(rename = "e", default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,

    /// A fingerprint of the query this cursor was minted against.
    #[serde(rename = "f")]
    pub fingerprint: String,

    /// Whatever the next page needs pinned. Per tool.
    #[serde(rename = "p")]
    pub pinned: P,
}

impl<P: Serialize + DeserializeOwned> ToolCursor<P> {
    /// Mints a cursor for the next page.
    pub fn new(engine: Option<&EngineCursor>, fingerprint: impl Into<String>, pinned: P) -> Self {
        Self {
            version: CURSOR_VERSION,
            engine: engine.map(|cursor| cursor.as_str().to_string()),
            fingerprint: fingerprint.into(),
            pinned,
        }
    }

    /// Encodes the cursor as the opaque string the model carries: base64url of compact JSON.
    pub fn encode(&self) -> Result<String, ToolError> {
        let json = serde_json::to_vec(self).map_err(|err| {
            ToolError::new(
                codes::INVALID_CURSOR,
                Remedy::RetryAfterChange,
                format!("The listing state could not be encoded: {err}"),
            )
        })?;

        Ok(URL_SAFE_NO_PAD.encode(json))
    }

    /// Decodes a cursor the model handed back, checking the version and the fingerprint.
    ///
    /// **A cursor that fails to decode, or whose fingerprint does not match, is a tool error
    /// saying so — never silently treated as end-of-results** (D32). The distinction matters:
    /// silently ending a listing shows the model less than exists and tells it nothing.
    pub fn decode(raw: &str, expected_fingerprint: &str) -> Result<Self, ToolError> {
        let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid_cursor())?;
        let cursor: Self = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;

        if cursor.version != CURSOR_VERSION {
            return Err(invalid_cursor());
        }

        if cursor.fingerprint != expected_fingerprint {
            return Err(ToolError::new(
                codes::INVALID_CURSOR,
                Remedy::RetryAfterChange,
                "That cursor belongs to a different search. A cursor can only continue the \
                 listing it came from.",
            )
            .with_next_step("start the listing again without a cursor"));
        }

        Ok(cursor)
    }

    /// The engine token to continue from, if any.
    pub fn engine_cursor(&self) -> Option<EngineCursor> {
        self.engine.as_ref().map(EngineCursor::new)
    }
}

/// The error for a cursor that does not decode or carries the wrong version.
fn invalid_cursor() -> ToolError {
    ToolError::new(
        codes::INVALID_CURSOR,
        Remedy::RetryAfterChange,
        "That cursor cannot be read. It may be from an older version of this server.",
    )
    .with_next_step("start the listing again without a cursor")
}

/// Walks every page of a listing, up to [`MAX_INTERNAL_PAGES`].
///
/// Internal pagination only — T1 walking every item type, say. It is **not** how a tool
/// paginates for the model: that is [`ToolCursor`], because the model must be able to stop.
///
/// The page cap bounds a runaway loop so it cannot hang a tool call. Hitting it is not silently
/// treated as the end of the data: the caller is told, because a truncated answer the model
/// cannot distinguish from a complete one is the failure mode this plan removes everywhere else.
pub async fn paginate_all<T, F, Fut>(mut fetch: F) -> Result<Vec<T>, ToolError>
where
    F: FnMut(Option<EngineCursor>) -> Fut,
    Fut: Future<Output = Result<ListPage<T>, ToolError>>,
{
    let mut collected = Vec::new();
    let mut cursor = None;

    for _ in 0..MAX_INTERNAL_PAGES {
        let page = fetch(cursor).await?;
        collected.extend(page.items);

        match page.next {
            Some(next) => cursor = Some(next),
            None => return Ok(collected),
        }
    }

    Err(ToolError::new(
        codes::CATALOG_UNAVAILABLE,
        Remedy::Retry,
        format!(
            "The listing did not finish within {MAX_INTERNAL_PAGES} pages, so this answer would              have been incomplete without saying so."
        ),
    )
    .with_details(serde_json::json!({ "pagesFetched": MAX_INTERNAL_PAGES })))
}

/// A stable fingerprint of whatever identifies a query.
///
/// FNV-1a over the canonical JSON, rather than `DefaultHasher`, because a cursor has to mean the
/// same thing after a restart and `DefaultHasher`'s output is explicitly not guaranteed stable
/// across Rust releases. A mismatch is a safe failure — the model is told to start again — but a
/// spurious one on every deploy would be a bad experience for no reason.
pub fn fingerprint(value: &serde_json::Value) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    // Canonicalised explicitly. The workspace enables `serde_json`'s `preserve_order`, so a
    // `Value`'s own rendering follows insertion order and two equal queries built in a different
    // order would fingerprint differently — refusing a cursor as "a different search". Sorting
    // keys here reproduces byte for byte what the `BTreeMap`-backed rendering used to give, so
    // cursors minted before the feature was turned on stay valid.
    let mut canonical = String::new();
    write_canonical(value, &mut canonical);

    let mut hash = FNV_OFFSET;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    format!("{hash:016x}")
}

/// Renders `value` as compact JSON with every object's keys in sorted order — the one rendering a
/// fingerprint may depend on.
fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();

            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                // A key is a JSON string; `Value::String`'s rendering is its escaped form.
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                if let Some(entry) = object.get(key) {
                    write_canonical(entry, out);
                }
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests;
