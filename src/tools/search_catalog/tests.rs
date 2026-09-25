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
    registry::contract::{CallContext, Tool},
    tools::search_catalog::{
        MAX_FILTER_ENTRIES, MAX_KIND_CANDIDATES, MAX_QUERY_BYTES, SearchCatalog,
        SearchCatalogInput, ast, cursor,
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item, mock_item_type_definition,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The global listing and its count.
const GLOBAL_PATH: &str = "/items";
const GLOBAL_COUNT_PATH: &str = "/items/count";

/// The family the `Service` fixture resolves to, and its count.
const FAMILY_PATH: &str = "/stable.example.com/v1/items/services";
const FAMILY_COUNT_PATH: &str = "/stable.example.com/v1/items/services/count";

/// The type listing, which is where `kind` is resolved and candidates are drawn from.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The point lookup a `kind: "Service"` resolution sends.
const SERVICE_LOOKUP: &str = "spec.names.kind=Service";

/// A port nothing listens on.
const UNREACHABLE_ENGINE: &str = "http://127.0.0.1:9";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// The `rawq` of the golden search — `query: "gateway"`, `labels: {env: prod}` — recorded.
/// **The regression anchor for the whole pipeline**: any change to the mapping, the translator
/// or the encoding moves it.
const GOLDEN_RAWQ: &str = "eyJhbmQiOlt7Im9yIjpbeyJtZXRhZGF0YS5uYW1lIjp7Im1hdGNoZXMiOiIvZ2F0ZXdheS9pIn19LHsibWV0YWRhdGEudGl0bGUiOnsibWF0Y2hlcyI6Ii9nYXRld2F5L2kifX0seyJtZXRhZGF0YS50YWdzIjp7Im1hdGNoZXMiOiIvZ2F0ZXdheS9pIn19XX0seyJtZXRhZGF0YS5sYWJlbHMuZW52Ijp7ImVxIjoicHJvZCJ9fV19";

/// The recorded size of a realistic full page (T2 §7). Regression detection, not a limit.
const RECORDED_FULL_PAGE_BYTES: usize = 8_061;

/// How far the full-page golden may drift.
const SIZE_TOLERANCE_PERCENT: usize = 1;

/// A search input, with everything absent.
fn mock_input() -> SearchCatalogInput {
    SearchCatalogInput {
        query: None,
        kind: None,
        labels: None,
        fields: None,
        limit: None,
        cursor: None,
    }
}

/// A one-entry map.
fn mock_map(key: &str, value: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(key.to_string(), value.to_string())])
}

/// `count` distinct items, `item-000`…, in the global listing's shape.
fn mock_items(count: usize, offset: usize) -> Vec<Value> {
    (offset..offset + count)
        .map(|index| mock_item(&format!("item-{index:03}")))
        .collect()
}

/// A `List` envelope with an optional continuation token.
fn mock_page(items: Vec<Value>, next: Option<&str>) -> Value {
    let mut metadata = json!({});
    if let Some(next) = next {
        metadata["continue"] = json!(next);
    }

    json!({ "apiVersion": "v1", "kind": "List", "metadata": metadata, "items": items })
}

/// A call context against `base_url`.
fn mock_context_at(base_url: &str, budget: Duration) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        Some("Bearer test-token"),
        None,
    ));
    let client = EngineClientFactory::new(
        base_url,
        "/",
        Duration::from_secs(5),
        Duration::from_millis(200),
        0,
    )
    .expect("the fixture URL is well formed")
    .bind(identity.clone(), Deadline::starting_now(budget));

    CallContext::new(
        client,
        Deadline::starting_now(budget),
        CancellationToken::new(),
        None,
        identity.tenant_key(),
    )
}

/// A call context pointed at `engine`.
fn mock_context(engine: &MockEngine) -> CallContext {
    mock_context_at(&engine.server().uri(), CALL_BUDGET)
}

/// Serves `page` at `listing_path`, to any query.
async fn mount_listing(engine: &MockEngine, listing_path: &str, page: Value) {
    Mock::given(method("GET"))
        .and(path(listing_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(page))
        .mount(engine.server())
        .await;
}

/// Serves `{count}` at `count_path`.
async fn mount_count(engine: &MockEngine, count_path: &str, count: u64) {
    Mock::given(method("GET"))
        .and(path(count_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": count })))
        .mount(engine.server())
        .await;
}

/// Makes `kind: "Service"` resolve to the fixture family.
async fn mount_service_type(engine: &MockEngine) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", SERVICE_LOOKUP))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
            vec![mock_item_type_definition(
                "Service",
                "services",
                "stable.example.com",
            )],
            None,
        )))
        .mount(engine.server())
        .await;
}

/// A type with a display name of its own — the shared fixture gives every type the same one,
/// which would make every type a near match for anything containing "service".
fn mock_type(kind: &str, plural: &str, display: &str) -> Value {
    let mut itd = mock_item_type_definition(kind, plural, "example.com");
    itd["spec"]["names"]["displayPlural"] = json!(display);
    itd
}

/// Runs the tool, returning the payload.
async fn run(context: &CallContext, input: SearchCatalogInput) -> Result<Value, ToolError> {
    SearchCatalog
        .call(context, input)
        .await
        .map(|output| output.payload().clone())
}

/// One request the engine received: its path, its raw query string and its `rawq` values.
struct Sent {
    path: String,
    query: String,
    rawq: Vec<String>,
}

/// Every request the engine received, in order.
async fn requests(engine: &MockEngine) -> Vec<Sent> {
    engine
        .server()
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| Sent {
            path: request.url.path().to_string(),
            query: request.url.query().unwrap_or_default().to_string(),
            rawq: request
                .url
                .query_pairs()
                .filter(|(key, _)| key == "rawq")
                .map(|(_, value)| value.into_owned())
                .collect(),
        })
        .collect()
}

/// The `name`s of a payload's rows.
fn names(payload: &Value) -> Vec<String> {
    payload["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// §4 — parameters → AST.
// ---------------------------------------------------------------------------------------------

/// `query` becomes an `or` of three `matches`, one literal shared, inside the `and` envelope.
#[rstest]
fn test_query_is_an_or_of_three_matches_in_an_and() {
    let predicate = ast::build(Some("gateway"), None, None)
        .expect("a valid search")
        .expect("a predicate");

    assert_eq!(
        predicate.to_json(),
        json!({ "and": [{ "or": [
            { "metadata.name":  { "matches": "/gateway/i" } },
            { "metadata.title": { "matches": "/gateway/i" } },
            { "metadata.tags":  { "matches": "/gateway/i" } }
        ] }] })
    );
}

/// Labels and fields become `eq`, in key order, after the query.
#[rstest]
fn test_labels_and_fields_are_eq_after_the_query() {
    let predicate = ast::build(
        Some("gateway"),
        Some(&mock_map("env", "prod")),
        Some(&mock_map("spec.replicas", "2")),
    )
    .expect("a valid search")
    .expect("a predicate");

    let clauses = predicate.to_json()["and"].clone();

    assert_eq!(
        clauses[1],
        json!({ "metadata.labels.env": { "eq": "prod" } })
    );
    assert_eq!(clauses[2], json!({ "spec.replicas": { "eq": "2" } }));
}

/// Nothing asked for is no predicate — and, below, no `rawq` on the wire.
#[rstest]
fn test_no_parameters_is_no_predicate() {
    assert!(ast::build(None, None, None).expect("valid").is_none());
}

/// Metacharacters, quotes and non-ASCII are **escaped** into the literal, never interpreted.
#[rstest]
fn test_query_text_is_escaped_not_interpreted() {
    let predicate = ast::build(Some(r#"a.b (c) "d" été"#), None, None)
        .expect("a valid search")
        .expect("a predicate");

    assert_eq!(
        predicate.to_json()["and"][0]["or"][0]["metadata.name"]["matches"],
        json!(r#"/a\.b \(c\) "d" été/i"#)
    );
}

/// **The golden base64** — the regression anchor for the whole pipeline.
#[rstest]
fn test_the_golden_search_encodes_to_its_recorded_rawq() {
    let rawq = ast::build(Some("gateway"), Some(&mock_map("env", "prod")), None)
        .expect("a valid search")
        .expect("a predicate")
        .encode_rawq()
        .expect("it encodes");

    assert_eq!(rawq, vec![GOLDEN_RAWQ.to_string()]);
}

/// Two searches whose maps the model wrote in different key orders are the **same** search:
/// byte-identical `rawq` and the same cursor fingerprint.
#[rstest]
fn test_key_order_does_not_change_the_rawq_or_the_fingerprint() {
    let first: SearchCatalogInput = serde_json::from_str(
        r#"{"labels":{"b":"2","a":"1"},"fields":{"spec.y":"1","spec.x":"2"}}"#,
    )
    .expect("valid input");
    let second: SearchCatalogInput = serde_json::from_str(
        r#"{"labels":{"a":"1","b":"2"},"fields":{"spec.x":"2","spec.y":"1"}}"#,
    )
    .expect("valid input");

    let encode = |input: &SearchCatalogInput| {
        let predicate = ast::build(None, input.labels.as_ref(), input.fields.as_ref())
            .expect("valid")
            .expect("a predicate");
        (
            predicate.encode_rawq().expect("it encodes"),
            cursor::fingerprint(None, Some(&predicate)),
        )
    };

    assert_eq!(encode(&first), encode(&second));
}

// ---------------------------------------------------------------------------------------------
// §5 — the engine calls: path selection, projection, no `rawq` when there is nothing to send.
// ---------------------------------------------------------------------------------------------

/// A search with no parameters returns the first page, sends no `rawq`, and does not error.
#[rstest]
#[tokio::test]
async fn test_a_search_with_no_parameters_lists_the_first_page() {
    let engine = MockEngine::start().await;
    mount_listing(&engine, GLOBAL_PATH, mock_page(mock_items(2, 0), None)).await;

    let payload = run(&mock_context(&engine), mock_input())
        .await
        .expect("an empty search is not an error");

    assert_eq!(names(&payload), vec!["item-000", "item-001"]);
    let sent = requests(&engine).await;
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].rawq.is_empty(),
        "no rawq for an empty search: {}",
        sent[0].query
    );
    assert!(
        sent[0].query.contains("limit=50"),
        "the engine's default page: {}",
        sent[0].query
    );
}

/// `kind` resolves once and selects the family endpoint; it is not a predicate.
#[rstest]
#[tokio::test]
async fn test_kind_selects_the_family_endpoint() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount_listing(&engine, FAMILY_PATH, mock_page(mock_items(1, 0), None)).await;

    let payload = run(
        &mock_context(&engine),
        SearchCatalogInput {
            kind: Some("Service".to_string()),
            query: Some("gateway".to_string()),
            ..mock_input()
        },
    )
    .await
    .expect("the search succeeds");

    assert_eq!(names(&payload), vec!["item-000"]);
    let sent = requests(&engine).await;
    let listing = sent
        .iter()
        .find(|sent| sent.path == FAMILY_PATH)
        .expect("the family endpoint was used");
    let rawq = &listing.rawq;
    assert_eq!(rawq.len(), 1);
    assert!(
        !listing.query.contains("field="),
        "T2-D1: rawq only, no field shortcut"
    );
    assert!(
        !String::from_utf8(
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &rawq[0])
                .expect("base64")
        )
        .expect("utf-8")
        .contains("Service"),
        "kind is the endpoint, never a predicate"
    );
}

/// A row carries everything needed to act on the item, from the partial projection alone.
#[rstest]
#[tokio::test]
async fn test_a_row_is_projected_from_the_partial_metadata() {
    let engine = MockEngine::start().await;
    mount_listing(
        &engine,
        GLOBAL_PATH,
        mock_page(vec![mock_item("api-gateway")], None),
    )
    .await;

    let payload = run(&mock_context(&engine), mock_input())
        .await
        .expect("the search succeeds");

    assert_eq!(
        serde_json::to_string(&payload["items"][0]).expect("a row serialises"),
        r#"{"name":"api-gateway","kind":"Service","title":"Example Service","group":"stable.example.com","version":"v1","family":"services","labels":{"environment":"demo"}}"#
    );
}

// ---------------------------------------------------------------------------------------------
// T2-D5 — `total` is conditional.
// ---------------------------------------------------------------------------------------------

/// A full page pays for a count, with the **identical** `rawq`.
#[rstest]
#[tokio::test]
async fn test_a_full_page_counts_with_the_identical_rawq() {
    let engine = MockEngine::start().await;
    mount_listing(
        &engine,
        GLOBAL_PATH,
        mock_page(mock_items(3, 0), Some("page-2")),
    )
    .await;
    mount_count(&engine, GLOBAL_COUNT_PATH, 128).await;

    let payload = run(
        &mock_context(&engine),
        SearchCatalogInput {
            query: Some("item".to_string()),
            limit: Some(3),
            ..mock_input()
        },
    )
    .await
    .expect("the search succeeds");

    assert_eq!(payload["total"], json!(128));
    let sent = requests(&engine).await;
    let listing = sent
        .iter()
        .find(|sent| sent.path == GLOBAL_PATH)
        .expect("listed");
    let count = sent
        .iter()
        .find(|sent| sent.path == GLOBAL_COUNT_PATH)
        .expect("counted");
    assert!(!listing.rawq.is_empty());
    assert_eq!(listing.rawq, count.rawq);
}

/// A page that is not full already says how many there are: no count call.
#[rstest]
#[tokio::test]
async fn test_a_partial_page_does_not_count() {
    let engine = MockEngine::start().await;
    mount_listing(&engine, GLOBAL_PATH, mock_page(mock_items(7, 0), None)).await;

    let payload = run(&mock_context(&engine), mock_input())
        .await
        .expect("the search succeeds");

    assert_eq!(payload["total"], json!(7));
    assert!(
        requests(&engine)
            .await
            .iter()
            .all(|sent| sent.path != GLOBAL_COUNT_PATH),
        "a partial page must not pay for a count"
    );
}

/// On a later page a partial page's `total` counts **every** page, not just this one — the
/// cursor carries how many came before.
#[rstest]
#[tokio::test]
async fn test_a_later_partial_page_totals_every_page() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(GLOBAL_PATH))
        .and(query_param_is_missing("continue"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(mock_items(3, 0), Some("page-2"))),
        )
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(GLOBAL_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(mock_items(2, 3), None)))
        .mount(engine.server())
        .await;
    mount_count(&engine, GLOBAL_COUNT_PATH, 5).await;
    let context = mock_context(&engine);

    let first = run(
        &context,
        SearchCatalogInput {
            limit: Some(3),
            ..mock_input()
        },
    )
    .await
    .expect("page one");
    let second = run(
        &context,
        SearchCatalogInput {
            limit: Some(3),
            cursor: first["cursor"].as_str().map(str::to_string),
            ..mock_input()
        },
    )
    .await
    .expect("page two");

    assert_eq!(names(&second), vec!["item-003", "item-004"]);
    assert_eq!(
        second["total"],
        json!(5),
        "3 earlier + 2 here, with no count call on page two"
    );
    assert!(
        second.get("cursor").is_none(),
        "the last page has no cursor"
    );
}

// ---------------------------------------------------------------------------------------------
// T2-D3 — `fields` validation differs by path.
// ---------------------------------------------------------------------------------------------

/// The global endpoint accepts any `spec.` path.
#[rstest]
#[tokio::test]
async fn test_any_spec_field_is_accepted_globally() {
    let engine = MockEngine::start().await;
    mount_listing(&engine, GLOBAL_PATH, mock_page(vec![], None)).await;

    run(
        &mock_context(&engine),
        SearchCatalogInput {
            fields: Some(mock_map("spec.anything.at.all", "x")),
            ..mock_input()
        },
    )
    .await
    .expect("any spec path is valid globally");
}

/// With `kind`, a `spec.` path must be selectable, and the valid ones are named.
#[rstest]
#[tokio::test]
async fn test_a_spec_field_not_selectable_for_the_kind_is_refused() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;

    let error = run(
        &mock_context(&engine),
        SearchCatalogInput {
            kind: Some("Service".to_string()),
            fields: Some(mock_map("spec.anything", "x")),
            ..mock_input()
        },
    )
    .await
    .expect_err("an unselectable path is refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["validPaths"].clone()),
        Some(json!(["spec.replicas"]))
    );
}

/// `metadata.*` fields are filterable on the family endpoint too — the asymmetry is `spec`'s.
#[rstest]
#[tokio::test]
async fn test_a_metadata_field_is_accepted_with_a_kind() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount_listing(&engine, FAMILY_PATH, mock_page(vec![], None)).await;

    run(
        &mock_context(&engine),
        SearchCatalogInput {
            kind: Some("Service".to_string()),
            fields: Some(mock_map("metadata.name", "api-gateway")),
            ..mock_input()
        },
    )
    .await
    .expect("a metadata field is valid with a kind");
}

// ---------------------------------------------------------------------------------------------
// §6 — the cursor.
// ---------------------------------------------------------------------------------------------

/// A cursor continues the listing it came from, and a `kind` search does not resolve again.
#[rstest]
#[tokio::test]
async fn test_a_cursor_continues_without_resolving_again() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    Mock::given(method("GET"))
        .and(path(FAMILY_PATH))
        .and(query_param_is_missing("continue"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(mock_items(2, 0), Some("page-2"))),
        )
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(FAMILY_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(mock_items(1, 2), None)))
        .mount(engine.server())
        .await;
    mount_count(&engine, FAMILY_COUNT_PATH, 3).await;
    let context = mock_context(&engine);
    let search = || SearchCatalogInput {
        kind: Some("Service".to_string()),
        limit: Some(2),
        ..mock_input()
    };

    let first = run(&context, search()).await.expect("page one");
    let second = run(
        &context,
        SearchCatalogInput {
            cursor: first["cursor"].as_str().map(str::to_string),
            ..search()
        },
    )
    .await
    .expect("page two");

    assert_eq!(names(&second), vec!["item-002"]);
    let lookups = requests(&engine)
        .await
        .iter()
        .filter(|sent| sent.path == TYPES_PATH)
        .count();
    assert_eq!(lookups, 1, "the kind is resolved on the first page only");
}

/// A cursor that is garbage, or belongs to a different search, is `invalid_cursor` — never an
/// empty page the model would read as the end of the results.
#[rstest]
#[case::garbage(|_first: &str| "not-a-cursor".to_string(), mock_map("env", "prod"))]
#[case::replayed_against_another_label(|first: &str| first.to_string(), mock_map("env", "dev"))]
#[tokio::test]
async fn test_a_bad_cursor_is_invalid_not_the_end(
    #[case] tamper: fn(&str) -> String,
    #[case] second_labels: BTreeMap<String, String>,
) {
    let engine = MockEngine::start().await;
    mount_listing(
        &engine,
        GLOBAL_PATH,
        mock_page(mock_items(1, 0), Some("page-2")),
    )
    .await;
    mount_count(&engine, GLOBAL_COUNT_PATH, 2).await;
    let context = mock_context(&engine);

    let first = run(
        &context,
        SearchCatalogInput {
            labels: Some(mock_map("env", "prod")),
            limit: Some(1),
            ..mock_input()
        },
    )
    .await
    .expect("page one");

    let error = run(
        &context,
        SearchCatalogInput {
            labels: Some(second_labels),
            limit: Some(1),
            cursor: Some(tamper(first["cursor"].as_str().expect("a cursor"))),
            ..mock_input()
        },
    )
    .await
    .expect_err("a bad cursor is an error, not an empty page");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_CURSOR, Remedy::RetryAfterChange)
    );
}

/// A cursor minted without `kind` cannot continue a search that adds one.
#[rstest]
#[tokio::test]
async fn test_a_global_cursor_cannot_continue_a_kind_search() {
    let engine = MockEngine::start().await;
    mount_listing(
        &engine,
        GLOBAL_PATH,
        mock_page(mock_items(1, 0), Some("page-2")),
    )
    .await;
    mount_count(&engine, GLOBAL_COUNT_PATH, 2).await;
    let context = mock_context(&engine);

    let first = run(
        &context,
        SearchCatalogInput {
            limit: Some(1),
            ..mock_input()
        },
    )
    .await
    .expect("page one");
    let error = run(
        &context,
        SearchCatalogInput {
            kind: Some("Service".to_string()),
            limit: Some(1),
            cursor: first["cursor"].as_str().map(str::to_string),
            ..mock_input()
        },
    )
    .await
    .expect_err("the kind is part of the search");

    assert_eq!(error.code, codes::INVALID_CURSOR);
}

// ---------------------------------------------------------------------------------------------
// T2-D6 — clamp and echo; T2-D7 — the page is never shortened.
// ---------------------------------------------------------------------------------------------

/// `limit` is clamped into the engine's range and echoed **only** when the clamp changed it.
#[rstest]
#[case::default(None, "limit=50", None)]
#[case::unchanged(Some(50), "limit=50", None)]
#[case::above_the_maximum(Some(500), "limit=200", Some(200))]
#[case::zero(Some(0), "limit=1", Some(1))]
#[tokio::test]
async fn test_limit_is_clamped_and_echoed_only_when_changed(
    #[case] requested: Option<u16>,
    #[case] sent: &str,
    #[case] echoed: Option<u32>,
) {
    let engine = MockEngine::start().await;
    mount_listing(&engine, GLOBAL_PATH, mock_page(vec![], None)).await;

    let payload = run(
        &mock_context(&engine),
        SearchCatalogInput {
            limit: requested,
            ..mock_input()
        },
    )
    .await
    .expect("the search succeeds");

    assert!(requests(&engine).await[0].query.contains(sent));
    assert_eq!(
        payload.get("limit").and_then(Value::as_u64),
        echoed.map(u64::from)
    );
}

/// Whatever the engine returned for the page is emitted whole, and the cursor points after it.
#[rstest]
#[tokio::test]
async fn test_the_page_is_never_shortened() {
    let engine = MockEngine::start().await;
    mount_listing(
        &engine,
        GLOBAL_PATH,
        mock_page(mock_items(3, 0), Some("after-three")),
    )
    .await;
    mount_count(&engine, GLOBAL_COUNT_PATH, 10).await;

    let payload = run(
        &mock_context(&engine),
        SearchCatalogInput {
            limit: Some(3),
            ..mock_input()
        },
    )
    .await
    .expect("the search succeeds");

    assert_eq!(names(&payload).len(), 3);
    assert!(payload["cursor"].is_string());
}

// ---------------------------------------------------------------------------------------------
// §8 — every row, asserting `code` **and** `remedy`.
// ---------------------------------------------------------------------------------------------

/// A `400` on the `rawq` we built is our defect: the model never supplies `rawq`.
#[rstest]
#[tokio::test]
async fn test_a_400_on_our_rawq_is_a_server_defect() {
    let engine = MockEngine::start().await;
    engine.get_error(GLOBAL_PATH, 400, "invalid rawq").await;

    let error = run(
        &mock_context(&engine),
        SearchCatalogInput {
            query: Some("gateway".to_string()),
            ..mock_input()
        },
    )
    .await
    .expect_err("a 400 fails the call");

    assert_eq!(
        (error.code, error.remedy),
        (codes::SERVER_DEFECT, Remedy::Escalate)
    );
}

/// A query no legal split can carry is `query_too_large`, before the engine is asked.
#[rstest]
#[tokio::test]
async fn test_a_query_too_large_to_split_is_refused() {
    let engine = MockEngine::start().await;
    let labels: BTreeMap<String, String> = (0..MAX_FILTER_ENTRIES)
        .map(|index| (format!("key{index:02}"), "v".repeat(500)))
        .collect();

    let error = run(
        &mock_context(&engine),
        SearchCatalogInput {
            labels: Some(labels),
            ..mock_input()
        },
    )
    .await
    .expect_err("the query cannot be carried");

    assert_eq!(
        (error.code, error.remedy),
        (codes::QUERY_TOO_LARGE, Remedy::RetryAfterChange)
    );
    assert!(
        requests(&engine).await.is_empty(),
        "refused before the engine"
    );
}

/// An unknown `kind` returns near matches, not a bare `404`.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_returns_candidates() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Servic"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![], None)))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param_is_missing("field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
            vec![
                mock_type("Service", "services", "Services"),
                mock_type("ServiceAccount", "serviceaccounts", "Service accounts"),
                mock_type("Template", "templates", "Templates"),
            ],
            None,
        )))
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context(&engine),
        SearchCatalogInput {
            kind: Some("Servic".to_string()),
            ..mock_input()
        },
    )
    .await
    .expect_err("an unknown kind is an error");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!(["Service", "ServiceAccount"]))
    );
    assert!(
        error
            .next_step
            .as_deref()
            .is_some_and(|step| step.contains("list_catalog_types"))
    );
}

/// At most `MAX_KIND_CANDIDATES` near matches come back.
#[rstest]
#[tokio::test]
async fn test_candidates_are_capped() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Thing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![], None)))
        .mount(engine.server())
        .await;
    let many = (0..MAX_KIND_CANDIDATES + 3)
        .map(|index| {
            mock_item_type_definition(
                &format!("Thing{index}"),
                &format!("thing{index}s"),
                "example.com",
            )
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param_is_missing("field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(many, None)))
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context(&engine),
        SearchCatalogInput {
            kind: Some("Thing".to_string()),
            ..mock_input()
        },
    )
    .await
    .expect_err("an unknown kind is an error");

    assert_eq!(
        error
            .details
            .as_deref()
            .and_then(|details| details["candidates"].as_array().map(Vec::len)),
        Some(MAX_KIND_CANDIDATES)
    );
}

/// Every boundary bound is `invalid_input` naming its parameter, before the engine is asked.
#[rstest]
#[case::query_too_long(SearchCatalogInput { query: Some("x".repeat(MAX_QUERY_BYTES + 1)), ..mock_input() }, "query")]
#[case::kind_malformed(SearchCatalogInput { kind: Some("not a kind".to_string()), ..mock_input() }, "kind")]
#[case::kind_too_long(SearchCatalogInput { kind: Some("K".repeat(129)), ..mock_input() }, "kind")]
#[case::too_many_labels(
    SearchCatalogInput {
        labels: Some((0..=MAX_FILTER_ENTRIES).map(|i| (format!("k{i}"), "v".to_string())).collect()),
        ..mock_input()
    },
    "labels"
)]
#[case::bad_label_key(SearchCatalogInput { labels: Some(mock_map("bad key", "v")), ..mock_input() }, "labels")]
#[case::label_value_too_long(SearchCatalogInput { labels: Some(mock_map("env", &"v".repeat(513))), ..mock_input() }, "labels")]
#[case::too_many_fields(
    SearchCatalogInput {
        fields: Some((0..=MAX_FILTER_ENTRIES).map(|i| (format!("spec.f{i}"), "v".to_string())).collect()),
        ..mock_input()
    },
    "fields"
)]
#[tokio::test]
async fn test_an_input_over_its_bound_is_refused_naming_it(
    #[case] input: SearchCatalogInput,
    #[case] parameter: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&mock_context(&engine), input)
        .await
        .expect_err("the input is refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["field"].clone()),
        Some(json!(parameter))
    );
    assert!(
        requests(&engine).await.is_empty(),
        "refused before the engine"
    );
}

/// A `fields` path outside the filterable grammar is refused before the engine, too.
#[rstest]
#[tokio::test]
async fn test_a_field_outside_the_grammar_is_invalid_input() {
    let engine = MockEngine::start().await;

    let error = run(
        &mock_context(&engine),
        SearchCatalogInput {
            fields: Some(mock_map("status.phase", "x")),
            ..mock_input()
        },
    )
    .await
    .expect_err("an unfilterable path is refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
}

/// Engine unavailability is `catalog_unavailable` — unavailable, not empty.
#[rstest]
#[case::server_error(500)]
#[case::unavailable(503)]
#[tokio::test]
async fn test_an_engine_failure_is_catalog_unavailable(#[case] status: u16) {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(GLOBAL_PATH))
        .respond_with(ResponseTemplate::new(status).set_body_json(mock_error_body(status, "boom")))
        .mount(engine.server())
        .await;

    let error = run(&mock_context(&engine), mock_input())
        .await
        .expect_err("an engine failure fails the call");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
}

/// An engine nobody answers for is unavailable too.
#[rstest]
#[tokio::test]
async fn test_an_unreachable_engine_is_catalog_unavailable() {
    let error = run(
        &mock_context_at(UNREACHABLE_ENGINE, CALL_BUDGET),
        mock_input(),
    )
    .await
    .expect_err("an unreachable engine fails the call");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
}

/// The call's own budget running out is `deadline_exceeded`.
#[rstest]
#[tokio::test]
async fn test_a_spent_deadline_is_deadline_exceeded() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(GLOBAL_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_page(vec![], None))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), Duration::from_millis(100)),
        mock_input(),
    )
    .await
    .expect_err("the deadline runs out first");

    assert_eq!(
        (error.code, error.remedy),
        (codes::DEADLINE_EXCEEDED, Remedy::Retry)
    );
}

/// An empty result is **never** an error, and echoes the filters as interpreted.
#[rstest]
#[tokio::test]
async fn test_an_empty_result_echoes_the_filters() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount_listing(&engine, FAMILY_PATH, mock_page(vec![], None)).await;

    let payload = run(
        &mock_context(&engine),
        SearchCatalogInput {
            query: Some("gatewy".to_string()),
            kind: Some("Service".to_string()),
            ..mock_input()
        },
    )
    .await
    .expect("an empty result is not an error");

    assert_eq!(
        payload,
        json!({ "items": [], "total": 0, "filters": { "query": "gatewy", "kind": "Service" } })
    );
}

// ---------------------------------------------------------------------------------------------
// §7 — the byte golden: regression detection, not a limit.
// ---------------------------------------------------------------------------------------------

/// A realistic full page — the default 50 rows, each with a title, a family and labels —
/// serialises to a recorded size.
#[rstest]
#[tokio::test]
async fn test_a_realistic_full_page_serialises_to_its_recorded_size() {
    let engine = MockEngine::start().await;
    mount_listing(
        &engine,
        GLOBAL_PATH,
        mock_page(mock_items(50, 0), Some("page-2")),
    )
    .await;
    mount_count(&engine, GLOBAL_COUNT_PATH, 1_284).await;

    let payload = run(&mock_context(&engine), mock_input())
        .await
        .expect("the search succeeds");
    let actual = serde_json::to_string(&payload).expect("serialises").len();

    let tolerance = RECORDED_FULL_PAGE_BYTES * SIZE_TOLERANCE_PERCENT / 100;
    assert!(
        actual.abs_diff(RECORDED_FULL_PAGE_BYTES) <= tolerance,
        "a full page serialises to {actual} B, recorded {RECORDED_FULL_PAGE_BYTES} B \
         (±{SIZE_TOLERANCE_PERCENT} %). Update the recording if the growth is intended."
    );
}
