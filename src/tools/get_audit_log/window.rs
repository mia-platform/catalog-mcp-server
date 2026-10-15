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
// The audit window: what a cursor pins so that the audit log's offset pages stay one walk. It is
// carried in the cursor, never kept on this server.
//
// The log grows at its head while it is read, and an entry can be written after newer ones with
// an older time — stamped when its request started, then buffered. So the window bounds the walk
// in time, and remembers the last entry it returned, so the rows a late entry pushes into the next
// page can be recognised and left out.

use catalog_client::models::AuditLogEntry;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// The furthest `skip` the engine accepts; it refuses anything beyond.
pub const MAX_SKIP: u64 = 10_000;

/// What a cursor pins between pages.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuditWindow {
    /// The offset of the next page.
    #[serde(rename = "s")]
    pub skip: u64,

    /// The exclusive upper bound every page is read under.
    #[serde(rename = "u")]
    pub until: String,

    /// The id of the last entry returned, so a page can leave out what was already seen.
    #[serde(rename = "l")]
    pub last: String,
}

/// What comes after a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// The listing is complete.
    End,

    /// Another page starts at this offset.
    More(u64),

    /// There is more, but past the offset the engine accepts.
    PastCap,
}

/// The bound page 1 pins: one second after its newest entry, or the caller's own bound if it is
/// earlier.
///
/// `occurredUntil` is exclusive and times are shown to the second, so a bound at the newest
/// entry's own second would leave the rest of that second out of every later page. A caller's
/// bound in the future would pin nothing at all, so it is narrowed the same way. `None` when the
/// newest entry's time cannot be read, in which case the caller's bound, if any, is all there is.
pub fn pin_until(newest: &str, caller_until: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    let after_newest = DateTime::parse_from_rfc3339(newest)
        .ok()
        .map(|time| time.with_timezone(&Utc) + Duration::seconds(1));

    match (after_newest, caller_until) {
        (Some(pinned), Some(caller)) => Some(pinned.min(caller)),
        (pinned, caller) => pinned.or(caller),
    }
}

/// An instant as the engine's `occurredUntil` takes it.
pub fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The page with the entries already returned left out: everything up to and including `last`,
/// when it appears. Entries written late inside the window push earlier rows down, and these are
/// the rows they pushed.
pub fn drop_seen(mut entries: Vec<AuditLogEntry>, last: &str) -> Vec<AuditLogEntry> {
    match entries.iter().position(|entry| entry.uid == last) {
        Some(position) => entries.split_off(position + 1),
        None => entries,
    }
}

/// What follows a page that asked for `limit` entries from `skip` and got `fetched`.
///
/// A short page is the end. With an exact `total`, reaching it is the end too; an inexact one is
/// only the engine's count cap, so only a short page ends the walk.
pub fn next_page(skip: u64, fetched: u64, limit: u64, total: u64, total_is_exact: bool) -> Next {
    let next = skip + fetched;

    if fetched < limit || (total_is_exact && next >= total) {
        Next::End
    } else if next > MAX_SKIP {
        Next::PastCap
    } else {
        Next::More(next)
    }
}
