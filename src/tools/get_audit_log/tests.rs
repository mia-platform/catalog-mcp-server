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
    registry::contract::{CallContext, Tool, ToolOutput},
    tools::get_audit_log::{
        GetAuditLog, GetAuditLogInput, MAX_FILTER_VALUES, filters_json,
        shape::{SYSTEM_CALLER, shape},
        window::{self, AuditWindow, MAX_SKIP, Next},
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    models::AuditLogEntry,
    ops::audit_log::AuditLogQuery,
    pagination::{ToolCursor, fingerprint},
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item_type_definition,
        mock_list_envelope,
    },
};
use configuration::tools::ToolDefaults;
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    http::Method,
    matchers::{method, path, query_param},
};

/// The audit log.
const AUDIT_PATH: &str = "/audit-log";

/// Its vocabulary.
const ACTIONS_PATH: &str = "/audit-log/actions";

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The fixture type definition's URN, as the engine files it.
const TYPE_URN: &str =
    "urn:mia-platform-catalog:mia-platform.eu:v1:ItemTypeDefinition:services.stable.example.com";

/// A fictional principal.
const ADA: &str = "019f1cb8-12ec-7f43-94d4-285b77930ce7";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// What a full page of 50 shaped entries serialises to, recorded.
const RECORDED_FULL_PAGE_BYTES: usize = 9_916;

/// How far a full page may drift from its recording before the test says so.
const SIZE_TOLERANCE_PERCENT: usize = 5;

/// A call context against `engine`, with no retries.
fn mock_context(engine: &MockEngine) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        Some("Bearer test-token"),
        None,
    ));
    let client = EngineClientFactory::new(
        &engine.server().uri(),
        "/",
        Duration::from_secs(5),
        Duration::from_millis(200),
        0,
    )
    .expect("the fixture URL is well formed")
    .bind(identity.clone(), Deadline::starting_now(CALL_BUDGET));

    CallContext::new(
        client,
        Deadline::starting_now(CALL_BUDGET),
        CancellationToken::new(),
        None,
        identity.tenant_key(),
    )
}

/// Runs the tool, returning its whole output.
async fn run_with(engine: &MockEngine, arguments: Value) -> Result<ToolOutput, ToolError> {
    let input: GetAuditLogInput =
        serde_json::from_value(arguments).expect("the fixture arguments deserialise");

    GetAuditLog::new(&ToolDefaults::default())
        .call(&mock_context(engine), input)
        .await
}

/// Runs the tool, returning its payload.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    run_with(engine, arguments)
        .await
        .map(|output| output.payload().clone())
}

/// An entry by Ada on an item, at `time`.
fn mock_entry(uid: &str, time: &str) -> Value {
    json!({
        "uid": uid,
        "timestamp": time,
        "durationMs": 12,
        "action": { "code": "item.upsert", "description": "Create or update an item",
                    "displayName": "Create or Update Item" },
        "request": { "method": "PUT", "path": "/stable.example.com/v1/items/services/payments-api",
                     "clientIp": "10.0.0.1", "userAgent": "test" },
        "caller": { "id": ADA, "type": "user", "displayName": "Ada Lovelace" },
        "resource": {
            "scope": "instance", "kind": "Item", "uid": "u-1",
            "urn": "urn:mia-platform-catalog:stable.example.com:v1:Service:payments-api",
            "typeUrn": TYPE_URN, "typeDisplayName": "Services", "family": "services",
            "displayName": "Payments API", "exists": true
        },
        "result": { "outcome": "success", "status": 200 }
    })
}

/// The audit envelope around `items`.
fn mock_page(items: Vec<Value>, skip: u64, limit: u64, total: u64, exact: bool) -> Value {
    json!({
        "metadata": { "skip": skip, "limit": limit, "total": total, "totalIsExact": exact },
        "items": items,
    })
}

/// `count` entries, newest first, one second apart, ending at `10:00:<count>`; ids `e<n>`.
fn mock_entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let second = count - index;
            mock_entry(
                &format!("e{index:03}"),
                &format!("2026-09-18T10:{:02}:{:02}Z", second / 60, second % 60),
            )
        })
        .collect()
}

/// Answers the audit log with `body` on the page at `skip`.
async fn mount_page(engine: &MockEngine, skip: u64, body: Value) {
    Mock::given(method("GET"))
        .and(path(AUDIT_PATH))
        .and(query_param("skip", skip.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(engine.server())
        .await;
}

/// Answers the type lookup for `Service` with `definition`.
async fn mount_type(engine: &MockEngine, definition: Value) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![definition], None)),
        )
        .mount(engine.server())
        .await;
}

/// Every audit-log request the mock received.
async fn audit_requests(engine: &MockEngine) -> Vec<wiremock::Request> {
    requests_on(engine, Method::GET, AUDIT_PATH).await
}

/// Every request of `verb` the mock received on `url_path`.
async fn requests_on(engine: &MockEngine, verb: Method, url_path: &str) -> Vec<wiremock::Request> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.method == verb && request.url.path() == url_path)
        .collect()
}

/// One query parameter of a request, when present.
fn param(request: &wiremock::Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// The window a cursor carries.
fn window_of(cursor: &str, filters: &Value) -> AuditWindow {
    ToolCursor::<AuditWindow>::decode(cursor, &fingerprint(filters))
        .expect("a cursor this tool minted")
        .pinned
}

/// The fingerprint filters of a call with no filters at the default page size.
fn default_filters() -> Value {
    filters_json(&AuditLogQuery {
        limit: 50,
        ..AuditLogQuery::default()
    })
}

/// The ids of a payload's entries… which the shaped entries do not carry: their times, then.
fn times(payload: &Value) -> Vec<String> {
    payload["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["time"].as_str().expect("a time").to_string())
        .collect()
}

/// An entry read from its fixture JSON.
fn entry(value: Value) -> AuditLogEntry {
    serde_json::from_value(value).expect("a well-formed entry")
}

/// The error's details.
fn details(error: &ToolError) -> Value {
    error.details.as_deref().cloned().unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// The window, first: the walk must neither lose nor repeat a row.
// ---------------------------------------------------------------------------------------------

/// Page 1 pins `until` one second after its newest entry; page 2 re-sends it with the next `skip`.
#[rstest]
#[tokio::test]
async fn test_page_one_pins_one_second_after_its_newest_entry() {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(mock_entries(50), 0, 50, 120, true)).await;

    let payload = run(&engine, json!({})).await.expect("page 1");

    let cursor = payload["cursor"].as_str().expect("a cursor");
    let window = window_of(cursor, &default_filters());
    assert_eq!(window.until, "2026-09-18T10:00:51Z");
    assert_eq!(window.skip, 50);
    assert_eq!(window.last, "e049");
    assert!(param(&audit_requests(&engine).await[0], "occurredUntil").is_none());
}

/// Entries recorded late between the pages — one at the head, one inside page 1's newest second —
/// push page 1's last two rows into page 2; they are left out, and the next `skip` still counts
/// them.
#[rstest]
#[tokio::test]
async fn test_late_entries_never_repeat_a_row() {
    let engine = MockEngine::start().await;
    let first = mock_entries(100);
    mount_page(
        &engine,
        0,
        mock_page(first[..50].to_vec(), 0, 50, 102, true),
    )
    .await;

    let page_one = run(&engine, json!({})).await.expect("page 1");
    let cursor = page_one["cursor"].as_str().expect("a cursor").to_string();

    // The two late entries sit above row 0 now: rows 48 and 49 are pushed to the head of page 2.
    let shifted: Vec<Value> = first[48..98].to_vec();
    Mock::given(method("GET"))
        .and(path(AUDIT_PATH))
        .and(query_param("skip", "50"))
        .and(query_param("occurredUntil", "2026-09-18T10:01:41Z"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(shifted, 50, 50, 102, true)),
        )
        .mount(engine.server())
        .await;

    let page_two = run(&engine, json!({ "cursor": cursor }))
        .await
        .expect("page 2");

    let seen: std::collections::HashSet<String> = times(&page_one).into_iter().collect();
    assert!(
        times(&page_two).iter().all(|time| !seen.contains(time)),
        "a row repeated"
    );
    assert_eq!(page_two["entries"].as_array().map(Vec::len), Some(48));
    let window = window_of(
        page_two["cursor"].as_str().expect("a cursor"),
        &default_filters(),
    );
    assert_eq!(window.skip, 100, "dropped rows still count in the offset");
    assert_eq!(window.last, "e097");
}

/// A caller's `until` in the future pins nothing by itself: it is narrowed the same way. An
/// earlier one is kept.
#[rstest]
#[case::future("2030-01-01", "2026-09-18T10:00:51Z")]
#[case::earlier("2026-09-18T10:00:30Z", "2026-09-18T10:00:30Z")]
#[tokio::test]
async fn test_a_callers_until_is_pinned_too(#[case] until: &str, #[case] pinned: &str) {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(mock_entries(50), 0, 50, 120, true)).await;

    let payload = run(&engine, json!({ "until": until }))
        .await
        .expect("page 1");

    let filters = filters_json(&AuditLogQuery {
        until: Some(until.to_string()),
        limit: 50,
        ..AuditLogQuery::default()
    });
    assert_eq!(
        window_of(payload["cursor"].as_str().expect("a cursor"), &filters).until,
        pinned
    );
}

/// The window rules on their own.
#[rstest]
#[case::short_page(0, 49, 50, 49, true, Next::End)]
#[case::exact_total_reached(50, 50, 50, 100, true, Next::End)]
#[case::more(0, 50, 50, 120, true, Next::More(50))]
#[case::inexact_total_needs_a_short_page(9_950, 50, 50, 10_000, false, Next::More(10_000))]
#[case::at_the_cap(9_950, 50, 50, 10_000, false, Next::More(MAX_SKIP))]
#[case::past_the_cap(10_000, 50, 50, 10_000, false, Next::PastCap)]
fn test_what_follows_a_page(
    #[case] skip: u64,
    #[case] fetched: u64,
    #[case] limit: u64,
    #[case] total: u64,
    #[case] exact: bool,
    #[case] expected: Next,
) {
    assert_eq!(
        window::next_page(skip, fetched, limit, total, exact),
        expected
    );
}

/// The last page's rows are dropped up to and including `last`, and only when it reappears.
#[rstest]
fn test_drop_seen_only_drops_what_reappears() {
    let page: Vec<AuditLogEntry> = mock_entries(5).into_iter().map(entry).collect();

    assert_eq!(window::drop_seen(page.clone(), "e001").len(), 3);
    assert_eq!(window::drop_seen(page, "elsewhere").len(), 5);
}

/// Past the 10 000 the engine pages through, there is no cursor, and a warning says why.
#[rstest]
#[tokio::test]
async fn test_past_the_cap_there_is_no_cursor_but_a_warning() {
    let engine = MockEngine::start().await;
    mount_page(
        &engine,
        0,
        mock_page(mock_entries(50), 0, 50, 10_000, false),
    )
    .await;
    let filters = default_filters();
    let cursor = ToolCursor::new(
        None,
        fingerprint(&filters),
        AuditWindow {
            skip: 10_000,
            until: "2026-09-18T10:00:51Z".into(),
            last: "x".into(),
        },
    )
    .encode()
    .expect("encodes");
    mount_page(
        &engine,
        10_000,
        mock_page(mock_entries(50), 10_000, 50, 10_000, false),
    )
    .await;

    let output = run_with(&engine, json!({ "cursor": cursor }))
        .await
        .expect("the last page");

    assert!(output.payload().get("cursor").is_none());
    assert_eq!(
        output.render(None)["warnings"],
        json!([super::PAST_CAP_WARNING])
    );
}

/// A cursor replayed with other filters, a malformed one, and one whose offset the engine would
/// refuse are all `invalid_cursor`.
#[rstest]
#[tokio::test]
async fn test_a_bad_cursor_is_refused() {
    let engine = MockEngine::start().await;
    let beyond = ToolCursor::new(
        None,
        fingerprint(&default_filters()),
        AuditWindow {
            skip: MAX_SKIP + 1,
            until: "2026-09-18T10:00:51Z".into(),
            last: "x".into(),
        },
    )
    .encode()
    .expect("encodes");

    for arguments in [
        json!({ "cursor": "not-a-cursor" }),
        json!({ "cursor": beyond.clone(), "outcome": "error" }),
        json!({ "cursor": beyond }),
    ] {
        let error = run(&engine, arguments).await.expect_err("refused");
        assert_eq!(
            (error.code, error.remedy),
            (codes::INVALID_CURSOR, Remedy::RetryAfterChange)
        );
    }
    assert!(audit_requests(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The page.
// ---------------------------------------------------------------------------------------------

/// An empty first page has no cursor; it echoes the query and says what the log does not record —
/// and never reads the tenant's settings.
#[rstest]
#[tokio::test]
async fn test_an_empty_page_says_what_is_not_recorded() {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    let payload = run(&engine, json!({ "outcome": "error" }))
        .await
        .expect("an empty page");

    assert_eq!(
        payload,
        json!({
            "entries": [], "total": 0, "totalIsExact": true,
            "filters": { "outcome": "error", "limit": 50 },
            "hint": super::EMPTY_HINT,
        })
    );
    assert!(super::EMPTY_HINT.contains("does not prove nothing happened"));
    assert!(
        requests_on(&engine, Method::GET, "/tenant-settings")
            .await
            .is_empty()
    );
}

/// `includeAuditLogReads` is never sent; the filters are, `principal` and `action` repeated.
#[rstest]
#[tokio::test]
async fn test_the_query_never_asks_for_audit_log_reads() {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    run(
        &engine,
        json!({
            "since": "2026-09-01", "until": "2026-09-18T10:00:00Z",
            "principal": [ADA, "019f1cb8-0000-7000-8000-000000000001"],
            "action": ["item.upsert", "item.delete"], "outcome": "success", "scope": "instance"
        }),
    )
    .await
    .expect("a page");

    let request = &audit_requests(&engine).await[0];
    let pairs: Vec<(String, String)> = request.url.query_pairs().into_owned().collect();
    assert!(pairs.iter().all(|(key, _)| key != "includeAuditLogReads"));
    assert_eq!(
        pairs.iter().filter(|(key, _)| key == "principal").count(),
        2
    );
    assert_eq!(pairs.iter().filter(|(key, _)| key == "action").count(), 2);
    assert_eq!(
        param(request, "occurredSince").as_deref(),
        Some("2026-09-01")
    );
    assert_eq!(param(request, "scope").as_deref(), Some("instance"));
}

/// A `limit` outside 1–200 is clamped and echoed; the configured default is the engine's 50.
#[rstest]
#[case::default(None, "50", None)]
#[case::clamped(Some(500), "200", Some(200))]
#[case::kept(Some(10), "10", None)]
#[tokio::test]
async fn test_the_page_size(
    #[case] asked: Option<u32>,
    #[case] sent: &str,
    #[case] echoed: Option<u32>,
) {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(mock_entries(3), 0, 50, 3, true)).await;

    let payload = run(&engine, json!({ "limit": asked }))
        .await
        .expect("a page");

    assert_eq!(
        param(&audit_requests(&engine).await[0], "limit").as_deref(),
        Some(sent)
    );
    assert_eq!(
        payload.get("limit").cloned(),
        echoed.map(|limit| json!(limit))
    );
}

/// A full page of 50 shaped entries serialises to its recorded size.
#[rstest]
#[tokio::test]
async fn test_a_full_page_serialises_to_its_recorded_size() {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(mock_entries(50), 0, 50, 1_284, true)).await;

    let payload = run(&engine, json!({})).await.expect("a page");
    let actual = serde_json::to_string(&payload).expect("serialises").len();

    let tolerance = RECORDED_FULL_PAGE_BYTES * SIZE_TOLERANCE_PERCENT / 100;
    assert!(
        actual.abs_diff(RECORDED_FULL_PAGE_BYTES) <= tolerance,
        "a full page serialises to {actual} B, recorded {RECORDED_FULL_PAGE_BYTES} B \
         (±{SIZE_TOLERANCE_PERCENT} %). Update the recording if the growth is intended."
    );
}

// ---------------------------------------------------------------------------------------------
// Addressing: never a URN from the model.
// ---------------------------------------------------------------------------------------------

/// `name` and `kind` send the item's URN, built from the type without reading the item.
#[rstest]
#[tokio::test]
async fn test_an_item_is_addressed_by_its_urn_without_reading_it() {
    let engine = MockEngine::start().await;
    mount_type(
        &engine,
        mock_item_type_definition("Service", "services", GROUP),
    )
    .await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    let output = run_with(
        &engine,
        json!({ "name": "payments-api", "kind": "Service" }),
    )
    .await
    .expect("a page");

    assert_eq!(
        param(&audit_requests(&engine).await[0], "resourceUrn").as_deref(),
        Some("urn:mia-platform-catalog:stable.example.com:v1:Service:payments-api")
    );
    assert!(
        requests_on(
            &engine,
            Method::GET,
            "/stable.example.com/v1/items/services/payments-api"
        )
        .await
        .is_empty()
    );
    assert!(output.render(None).get("warnings").is_none());
}

/// A type serving several versions warns that only one version's history is shown.
#[rstest]
#[tokio::test]
async fn test_a_multi_version_type_warns_which_version() {
    let engine = MockEngine::start().await;
    let mut definition = mock_item_type_definition("Service", "services", GROUP);
    let mut second = definition["spec"]["versions"][0].clone();
    second["name"] = json!("v2");
    definition["spec"]["versions"]
        .as_array_mut()
        .expect("versions")
        .push(second);
    mount_type(&engine, definition).await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    let output = run_with(
        &engine,
        json!({ "name": "payments-api", "kind": "Service" }),
    )
    .await
    .expect("a page");

    let warnings = output.render(None)["warnings"].clone();
    assert!(
        warnings[0]
            .as_str()
            .is_some_and(|text| text.contains("several versions"))
    );
}

/// `kind` alone sends the type definition's URN, as the engine files it, and needs no served
/// version.
#[rstest]
#[tokio::test]
async fn test_a_type_is_addressed_by_its_definitions_urn() {
    let engine = MockEngine::start().await;
    let mut definition = mock_item_type_definition("Service", "services", GROUP);
    definition["spec"]["versions"][0]["served"] = json!(false);
    mount_type(&engine, definition).await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    run(&engine, json!({ "kind": "Service" }))
        .await
        .expect("a page");

    assert_eq!(
        param(&audit_requests(&engine).await[0], "resourceTypeUrn").as_deref(),
        Some(TYPE_URN)
    );
}

/// An item of a type with no served version cannot be addressed.
#[rstest]
#[tokio::test]
async fn test_an_item_of_an_unserved_type_is_unaddressable() {
    let engine = MockEngine::start().await;
    let mut definition = mock_item_type_definition("Service", "services", GROUP);
    definition["spec"]["versions"][0]["served"] = json!(false);
    mount_type(&engine, definition).await;

    let error = run(
        &engine,
        json!({ "name": "payments-api", "kind": "Service" }),
    )
    .await
    .expect_err("no served version");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNADDRESSABLE_TYPE, Remedy::Escalate)
    );
}

/// A type that does not resolve — deleted, say — is `not_found`, pointing at its delete entry.
#[rstest]
#[tokio::test]
async fn test_a_missing_type_points_at_its_delete_entry() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "kind": "Gone" }))
        .await
        .expect_err("no such type");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("itd.delete"))
    );
    assert!(audit_requests(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Shaping.
// ---------------------------------------------------------------------------------------------

/// The common entry: every field, `whoId` beside the name, the request's forensics left out.
#[rstest]
fn test_an_entry_is_shaped() {
    let shaped = serde_json::to_value(shape(entry(mock_entry("e1", "2026-09-18T10:30:45Z"))))
        .expect("serialises");

    assert_eq!(
        shaped,
        json!({
            "time": "2026-09-18T10:30:45Z", "action": "item.upsert", "who": "Ada Lovelace",
            "whoId": ADA, "what": "Payments API", "kind": "Service", "outcome": "success",
            "status": 200
        })
    );
}

/// Each fallback: no caller, an unresolved caller, a refused call with and without its error, a
/// resource that does not exist now, and the kinds and scopes the engine gives no name.
#[rstest]
#[case::no_caller("caller", Value::Null, "who", json!(SYSTEM_CALLER))]
#[case::unresolved_caller("caller", json!({ "id": ADA }), "who", json!(ADA))]
#[case::not_existing("resource.exists", json!(false), "exists", json!(false))]
#[case::webhook(
    "resource",
    json!({ "scope": "instance", "kind": "Webhook", "typeDisplayName": "Services", "exists": true }),
    "what",
    json!("webhook of Services")
)]
#[case::tenant_settings(
    "resource",
    json!({ "scope": "instance", "kind": "TenantSettings", "exists": true }),
    "what",
    json!("tenant settings")
)]
#[case::whole_type(
    "resource",
    json!({ "scope": "collection", "kind": "ItemTypeDefinition", "typeDisplayName": "Services",
            "exists": true }),
    "what",
    json!("Services")
)]
#[case::tenant_wide(
    "resource",
    json!({ "scope": "none", "exists": true }),
    "what",
    json!("Create or update an item")
)]
#[case::refused(
    "result",
    json!({ "outcome": "error", "status": 409,
            "error": { "status": 409, "error": "Conflict", "message": "Concurrent modification" } }),
    "error",
    json!("Concurrent modification")
)]
fn test_each_fallback(
    #[case] field: &str,
    #[case] value: Value,
    #[case] shaped_field: &str,
    #[case] expected: Value,
) {
    let mut raw = mock_entry("e1", "2026-09-18T10:30:45Z");
    match field.split_once('.') {
        Some((outer, inner)) => raw[outer][inner] = value,
        None if value.is_null() => {
            raw.as_object_mut().expect("an object").remove(field);
        }
        None => raw[field] = value,
    }

    let shaped = serde_json::to_value(shape(entry(raw))).expect("serialises");

    assert_eq!(shaped[shaped_field], expected, "{shaped}");
    if field == "caller" && expected == json!(SYSTEM_CALLER) {
        assert!(shaped.get("whoId").is_none());
    }
}

/// A truncated error body names no reason; a success carries no `error` and no `exists`.
#[rstest]
fn test_a_truncated_error_has_no_reason() {
    let mut raw = mock_entry("e1", "2026-09-18T10:30:45Z");
    raw["result"] = json!({ "outcome": "error", "status": 500,
                            "error": { "truncated": true, "originalSizeBytes": 9000 } });

    let shaped = serde_json::to_value(shape(entry(raw))).expect("serialises");

    assert!(shaped.get("error").is_none());
    assert!(shaped.get("exists").is_none());
}

/// The engine's `details` pass through as recorded: an auditing change says which way it went.
#[rstest]
fn test_details_are_kept() {
    let mut raw = mock_entry("e1", "2026-09-17T08:00:00Z");
    raw["action"]["code"] = json!("tenantSettings.update");
    raw["resource"] = json!({ "scope": "instance", "kind": "TenantSettings", "exists": true });
    raw["details"] = json!({ "from": true, "to": false });

    let shaped = serde_json::to_value(shape(entry(raw))).expect("serialises");

    assert_eq!(shaped["details"], json!({ "from": true, "to": false }));
    assert_eq!(shaped["what"], json!("tenant settings"));
}

// ---------------------------------------------------------------------------------------------
// Refusals.
// ---------------------------------------------------------------------------------------------

/// An unknown action fetches the vocabulary — only then — and lists the codes that exist, without
/// the audit log's own.
#[rstest]
#[tokio::test]
async fn test_an_unknown_action_lists_the_codes_that_exist() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(AUDIT_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(mock_error_body(
            400,
            "invalid query parameter 'action': unknown action code 'item.update'",
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(ACTIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "code": "item.upsert", "description": "d", "displayName": "Create or Update Item" },
            { "code": "auditLog.list", "description": "d", "displayName": "Read audit log" },
        ])))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "action": ["item.update"] }))
        .await
        .expect_err("unknown code");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        details(&error)["validActions"],
        json!([{ "code": "item.upsert", "displayName": "Create or Update Item" }])
    );
}

/// A `400` while only known codes were given is about something else: relayed, with this tool's
/// names for the engine's parameters.
#[rstest]
#[tokio::test]
async fn test_another_bad_request_is_relayed_with_the_tools_names() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(AUDIT_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(mock_error_body(
            400,
            "invalid query parameter 'occurredSince': must be an ISO 8601 date",
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(ACTIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "code": "item.upsert", "description": "d", "displayName": "Create or Update Item" },
        ])))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "action": ["item.upsert"] }))
        .await
        .expect_err("refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert!(
        error.message.contains("'occurredSince' (`since`)"),
        "{}",
        error.message
    );
}

/// A successful page never reads the vocabulary.
#[rstest]
#[tokio::test]
async fn test_a_success_never_reads_the_vocabulary() {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    run(&engine, json!({ "action": ["item.upsert"] }))
        .await
        .expect("a page");

    assert!(
        requests_on(&engine, Method::GET, ACTIONS_PATH)
            .await
            .is_empty()
    );
}

/// A `400` on page 1 is the caller's; on a later page it is this server's.
#[rstest]
#[case::first_page(false, codes::INVALID_INPUT, Remedy::RetryAfterChange)]
#[case::later_page(true, codes::SERVER_DEFECT, Remedy::Escalate)]
#[tokio::test]
async fn test_whose_bad_request_it_is(
    #[case] with_cursor: bool,
    #[case] code: &str,
    #[case] remedy: Remedy,
) {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(AUDIT_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(mock_error_body(400, "refused")))
        .mount(engine.server())
        .await;
    let arguments = if with_cursor {
        let cursor = ToolCursor::new(
            None,
            fingerprint(&default_filters()),
            AuditWindow {
                skip: 50,
                until: "2026-09-18T10:00:51Z".into(),
                last: "e049".into(),
            },
        )
        .encode()
        .expect("encodes");
        json!({ "cursor": cursor })
    } else {
        json!({})
    };

    let error = run(&engine, arguments).await.expect_err("refused");

    assert_eq!((error.code, error.remedy), (code, remedy));
}

/// Every other engine refusal keeps the client's mapping; a `403` says what permission is missing.
#[rstest]
#[case::forbidden(403, codes::FORBIDDEN, Remedy::Escalate)]
#[case::unavailable(503, codes::CATALOG_UNAVAILABLE, Remedy::Retry)]
#[tokio::test]
async fn test_the_other_refusals(#[case] status: u16, #[case] code: &str, #[case] remedy: Remedy) {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(AUDIT_PATH))
        .respond_with(ResponseTemplate::new(status).set_body_json(mock_error_body(status, "no")))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({})).await.expect_err("refused");

    assert_eq!((error.code, error.remedy), (code, remedy));
    if status == 403 {
        assert_eq!(error.message, super::FORBIDDEN_MESSAGE);
    }
}

/// A `403` from the type lookup keeps the client's own message: it is not about the audit log.
#[rstest]
#[tokio::test]
async fn test_a_forbidden_type_lookup_is_not_about_the_audit_log() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(403).set_body_json(mock_error_body(403, "no")))
        .mount(engine.server())
        .await;

    let error = run(&engine, json!({ "kind": "Service" }))
        .await
        .expect_err("refused");

    assert_eq!(error.code, codes::FORBIDDEN);
    assert_ne!(error.message, super::FORBIDDEN_MESSAGE);
}

/// The input bounds: refused before anything is sent, naming the parameter.
#[rstest]
#[case::name_without_kind(json!({ "name": "payments-api" }), "name")]
#[case::bad_name(json!({ "name": "Not A Name", "kind": "Service" }), "name")]
#[case::bad_kind(json!({ "kind": "9lives" }), "kind")]
#[case::group_without_kind(json!({ "group": GROUP }), "group")]
#[case::bad_date(json!({ "since": "yesterday" }), "since")]
#[case::inverted(json!({ "since": "2026-09-18", "until": "2026-09-17T23:00:00+02:00" }), "since")]
#[case::bad_principal(json!({ "principal": ["ada@example.com"] }), "principal")]
#[case::too_many_actions(
    json!({ "action": (0..=MAX_FILTER_VALUES).map(|i| format!("a{i}")).collect::<Vec<_>>() }),
    "action"
)]
#[case::audit_log_reads(json!({ "action": ["auditLog.list"] }), "action")]
#[tokio::test]
async fn test_a_broken_bound_is_refused_before_sending(
    #[case] arguments: Value,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&engine, arguments).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(details(&error)["field"], json!(field));
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .expect("the mock records its requests")
            .is_empty()
    );
}

/// A bare date and a date-time with an offset are compared as the engine compares them.
#[rstest]
#[tokio::test]
async fn test_dates_are_compared_as_instants() {
    let engine = MockEngine::start().await;
    mount_page(&engine, 0, mock_page(vec![], 0, 50, 0, true)).await;

    // Midnight UTC on the 18th is 02:00 in +02:00: the window is two hours, not empty.
    run(
        &engine,
        json!({ "since": "2026-09-18", "until": "2026-09-18T04:00:00+02:00" }),
    )
    .await
    .expect("a valid window");
}
