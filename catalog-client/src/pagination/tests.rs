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
    error::{Remedy, codes},
    models::{ListEnvelope, ListMetadata},
    pagination::{
        EngineCursor, ListPage, MAX_INTERNAL_PAGES, ToolCursor, fingerprint, paginate_all,
    },
};
use rstest::rstest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cell::RefCell;

/// What one tool pins across pages, standing in for a real tool's own shape.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct MockPinned {
    #[serde(rename = "u")]
    until: String,
}

fn mock_pinned() -> MockPinned {
    MockPinned {
        until: "2026-09-01T00:00:00Z".to_string(),
    }
}

fn mock_fingerprint() -> String {
    fingerprint(&json!({ "kind": "Service", "query": "gateway" }))
}

// ---------------------------------------------------------------------------------------------
// The engine envelope.
// ---------------------------------------------------------------------------------------------

/// `metadata.continue` is omitted on the last page, and that is how the end is signalled.
#[rstest]
fn test_an_envelope_without_a_continue_token_is_the_last_page() {
    let envelope = ListEnvelope {
        api_version: "mia-platform.eu/v1".to_string(),
        kind: "List".to_string(),
        metadata: ListMetadata::default(),
        items: vec![1, 2, 3],
    };

    let page = ListPage::from_envelope(envelope);

    assert_eq!(page.items, vec![1, 2, 3]);
    assert_eq!(page.next, None);
}

#[rstest]
fn test_a_continue_token_becomes_the_next_cursor() {
    let envelope = ListEnvelope {
        api_version: "mia-platform.eu/v1".to_string(),
        kind: "List".to_string(),
        metadata: ListMetadata {
            continue_token: Some("engine-token-2".to_string()),
        },
        items: vec![1],
    };

    let page = ListPage::from_envelope(envelope);

    assert_eq!(page.next, Some(EngineCursor::new("engine-token-2")));
}

// ---------------------------------------------------------------------------------------------
// D32 — the cursor is ours.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_a_cursor_round_trips() {
    let fp = mock_fingerprint();
    let cursor = ToolCursor::new(
        Some(&EngineCursor::new("engine-token-2")),
        &fp,
        mock_pinned(),
    );

    let encoded = cursor.encode().expect("a serialisable cursor");
    let decoded: ToolCursor<MockPinned> =
        ToolCursor::decode(&encoded, &fp).expect("our own cursor decodes");

    assert_eq!(decoded, cursor);
    assert_eq!(
        decoded.engine_cursor(),
        Some(EngineCursor::new("engine-token-2"))
    );
}

/// The engine's token must never be what the model holds: it would leak an internal format and
/// pin nothing else.
#[rstest]
fn test_the_engine_token_is_not_what_the_model_receives() {
    let encoded = ToolCursor::new(
        Some(&EngineCursor::new("engine-token-2")),
        mock_fingerprint(),
        mock_pinned(),
    )
    .encode()
    .expect("a serialisable cursor");

    assert_ne!(encoded, "engine-token-2");
    assert!(!encoded.contains("engine-token-2"));
}

/// **Never** silently treated as end-of-results.
#[rstest]
#[case::not_base64("!!! not base64 !!!")]
#[case::not_json("bm90IGpzb24")]
#[case::empty("")]
fn test_an_undecodable_cursor_is_a_tool_error(#[case] raw: &str) {
    let error = ToolCursor::<MockPinned>::decode(raw, &mock_fingerprint())
        .expect_err("an undecodable cursor is refused");

    assert_eq!(error.code, codes::INVALID_CURSOR);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error.next_step.as_deref(),
        Some("start the listing again without a cursor")
    );
}

#[rstest]
fn test_a_cursor_from_an_older_format_is_refused() {
    let raw = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        serde_json::to_vec(&json!({ "v": 0, "f": mock_fingerprint(), "p": mock_pinned() }))
            .expect("serialisable"),
    );

    let error = ToolCursor::<MockPinned>::decode(&raw, &mock_fingerprint())
        .expect_err("a wrong-version cursor is refused");

    assert_eq!(error.code, codes::INVALID_CURSOR);
}

/// A cursor cannot be replayed against a different query — which is the whole reason it carries
/// a fingerprint.
#[rstest]
fn test_a_cursor_cannot_be_replayed_against_another_query() {
    let encoded = ToolCursor::new(None, mock_fingerprint(), mock_pinned())
        .encode()
        .expect("a serialisable cursor");

    let other = fingerprint(&json!({ "kind": "Service", "query": "database" }));

    let error = ToolCursor::<MockPinned>::decode(&encoded, &other)
        .expect_err("a replayed cursor is refused");

    assert_eq!(error.code, codes::INVALID_CURSOR);
    assert!(error.message.contains("different search"));
}

/// The fingerprint has to survive a restart, so it cannot depend on a per-process hash seed.
#[rstest]
fn test_the_fingerprint_is_stable_and_order_independent() {
    let first = fingerprint(&json!({ "a": 1, "b": 2 }));
    let second = fingerprint(&json!({ "b": 2, "a": 1 }));

    assert_eq!(first, second);
    assert_eq!(first, fingerprint(&json!({ "a": 1, "b": 2 })));
    assert_ne!(first, fingerprint(&json!({ "a": 1, "b": 3 })));
}

// ---------------------------------------------------------------------------------------------
// Internal pagination.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_paginate_all_walks_every_page() {
    let pages = RefCell::new(vec![
        ListPage {
            items: vec![1, 2],
            next: Some(EngineCursor::new("p2")),
        },
        ListPage {
            items: vec![3, 4],
            next: Some(EngineCursor::new("p3")),
        },
        ListPage {
            items: vec![5],
            next: None,
        },
    ]);

    let collected = paginate_all(|_cursor| {
        let page = pages.borrow_mut().remove(0);
        async move { Ok(page) }
    })
    .await
    .expect("three pages walk cleanly");

    assert_eq!(collected, vec![1, 2, 3, 4, 5]);
}

#[rstest]
#[tokio::test]
async fn test_paginate_all_passes_the_previous_cursor_on() {
    let seen = RefCell::new(Vec::<Option<String>>::new());
    let remaining = RefCell::new(2);

    let _ = paginate_all(|cursor: Option<EngineCursor>| {
        seen.borrow_mut()
            .push(cursor.map(|c| c.as_str().to_string()));
        let mut left = remaining.borrow_mut();
        *left -= 1;
        let last = *left == 0;

        async move {
            Ok(ListPage {
                items: vec![0_u8],
                next: (!last).then(|| EngineCursor::new("p2")),
            })
        }
    })
    .await
    .expect("two pages walk cleanly");

    assert_eq!(seen.into_inner(), vec![None, Some("p2".to_string())]);
}

/// The cap bounds a runaway loop — and says so, rather than returning a truncated answer the
/// model cannot tell from a complete one.
#[rstest]
#[tokio::test]
async fn test_paginate_all_refuses_to_truncate_silently() {
    let error = paginate_all(|_cursor: Option<EngineCursor>| async move {
        Ok(ListPage {
            items: vec![0_u8],
            next: Some(EngineCursor::new("forever")),
        })
    })
    .await
    .expect_err("a runaway listing is refused");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert_eq!(
        error.details.expect("the page count is reported")["pagesFetched"],
        json!(MAX_INTERNAL_PAGES)
    );
}

#[rstest]
#[tokio::test]
async fn test_paginate_all_propagates_a_page_failure() {
    let error = paginate_all(|_cursor: Option<EngineCursor>| async move {
        Err::<ListPage<u8>, _>(crate::error::transport_failure(
            crate::error::Dispatched::No,
            None,
        ))
    })
    .await
    .expect_err("a failing page fails the walk");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
}
