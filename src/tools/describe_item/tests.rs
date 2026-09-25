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
    tools::describe_item::{
        DescribeItem, DescribeItemInput, Direction, GroupBy, MAX_AMBIGUOUS_CANDIDATES,
        MAX_NAME_BYTES,
        shape::{self, Grouping},
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    models::{ItemRelationshipEntry, RelationshipDirection},
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item, mock_item_type_definition,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The described item, in the `Service` fixture family.
const ITEM: &str = "api-gateway";
const ITEM_PATH: &str = "/stable.example.com/v1/items/services/api-gateway";
const RELATIONSHIPS_PATH: &str =
    "/bff/stable.example.com/v1/items/services/api-gateway/relationships";

/// The global listing the kindless probe goes through, and the type listing `kind` resolves in.
const GLOBAL_PATH: &str = "/items";
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// How long each arm is delayed in the concurrency test, and the most the pair may take together.
const ARM_DELAY: Duration = Duration::from_millis(400);
const CONCURRENT_CEILING: Duration = Duration::from_millis(700);

/// The recorded size of the 50-relationship fixture's response (T3 §6). Regression detection,
/// not a limit.
const RECORDED_FIFTY_RELATIONSHIPS_BYTES: usize = 3_310;

/// How far that golden may drift.
const SIZE_TOLERANCE_PERCENT: usize = 1;

/// The most a realistic shaped entry may weigh (T3-D2's ~83 B, with room for longer names).
const MAX_SHAPED_ENTRY_BYTES: usize = 100;

/// The URN the engine builds for an item.
fn urn(group: &str, kind: &str, name: &str) -> String {
    format!("urn:mia-platform-catalog:{group}:v1:{kind}:{name}")
}

/// The described item's URN.
fn item_urn() -> String {
    urn("stable.example.com", "Service", ITEM)
}

/// One BFF entry: the full relationship record, and the other end when `resolved`.
fn mock_entry(direction: &str, other: &str, relationship_type: &str, resolved: bool) -> Value {
    let other_urn = urn("stable.example.com", "Service", other);
    let (source, target) = match direction {
        "outbound" => (item_urn(), other_urn),
        _ => (other_urn, item_urn()),
    };
    let mut entry = json!({
        "direction": direction,
        "relationship": {
            "apiVersion": "mia-platform.eu/v1",
            "kind": "Relationship",
            "metadata": {
                "name": format!("{ITEM}-{relationship_type}-{other}"),
                "family": "relationships",
                "uid": "550e8400-e29b-41d4-a716-446655440000",
                "urn": urn("mia-platform.eu", "Relationship", "r"),
                "creationTimestamp": "2026-09-17T10:30:45Z",
                "updateTimestamp": "2026-09-17T10:30:45Z"
            },
            "spec": {
                "sourceRef": source,
                "targetRef": target,
                "typeRef": urn("mia-platform.eu", "RelationshipType", relationship_type)
            },
            "resourceVersion": "1"
        }
    });
    if resolved {
        entry["relatedItem"] = json!({
            "apiVersion": "stable.example.com/v1",
            "kind": "Service",
            "metadata": { "name": other, "family": "services" }
        });
    }
    entry
}

/// Parses BFF entries the way the client does.
fn entries(raw: Vec<Value>) -> Vec<ItemRelationshipEntry> {
    raw.into_iter()
        .map(|entry| serde_json::from_value(entry).expect("a well-formed fixture entry"))
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

/// A describe input for `name`, everything else at its default.
fn mock_input(name: &str) -> DescribeItemInput {
    serde_json::from_value(json!({ "name": name })).expect("a valid input")
}

/// The same, with `kind: "Service"`.
fn mock_kinded_input() -> DescribeItemInput {
    DescribeItemInput {
        kind: Some("Service".to_string()),
        ..mock_input(ITEM)
    }
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

/// Serves `body` at `at`, with an optional delay.
async fn mount(engine: &MockEngine, at: &str, status: u16, body: Value, delay: Option<Duration>) {
    let mut response = ResponseTemplate::new(status).set_body_json(body);
    if let Some(delay) = delay {
        response = response.set_delay(delay);
    }

    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Makes `kind: "Service"` resolve to the fixture family.
async fn mount_service_type(engine: &MockEngine) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
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

/// The item, and a two-entry relationships page — the happy path of a kinded call.
async fn mount_happy_path(engine: &MockEngine) {
    mount_service_type(engine).await;
    mount(engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    mount(
        engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(
            vec![
                mock_entry("outbound", "auth-service", "depends-on", true),
                mock_entry("inbound", "frontend", "depends-on", true),
            ],
            None,
        ),
        None,
    )
    .await;
}

/// Runs the tool, returning its output.
async fn run_output(
    context: &CallContext,
    input: DescribeItemInput,
) -> Result<ToolOutput, ToolError> {
    DescribeItem.call(context, input).await
}

/// Runs the tool, returning the payload.
async fn run(context: &CallContext, input: DescribeItemInput) -> Result<Value, ToolError> {
    run_output(context, input)
        .await
        .map(|output| output.payload().clone())
}

/// Every `(path, query)` the engine received.
async fn requests(engine: &MockEngine) -> Vec<(String, String)> {
    engine
        .server()
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| {
            (
                request.url.path().to_string(),
                request.url.query().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// §8 — the shaper (T3-D2, T3-D7).
// ---------------------------------------------------------------------------------------------

/// A BFF entry becomes `{name, kind, type}`, `type` the last segment of `typeRef`.
#[rstest]
fn test_an_entry_is_shaped_to_its_four_fields() {
    let shaped = shape::group(
        &entries(vec![mock_entry(
            "outbound",
            "auth-service",
            "depends-on",
            true,
        )]),
        Grouping::ByDirection,
        None,
    );

    assert_eq!(
        serde_json::to_string(&shaped["outbound"][0]).expect("serialises"),
        r#"{"name":"auth-service","kind":"Service","type":"depends-on"}"#
    );
}

/// An entry whose other end is unresolved is **reported, never dropped** — and says nothing
/// about why.
#[rstest]
fn test_an_unresolved_entry_is_reported_not_dropped() {
    let shaped = shape::group(
        &entries(vec![
            mock_entry("outbound", "auth-service", "depends-on", true),
            mock_entry("outbound", "gone-service", "depends-on", false),
        ]),
        Grouping::ByDirection,
        None,
    );

    let outbound = shaped["outbound"].as_array().expect("an array");
    assert_eq!(outbound.len(), 2, "nothing is dropped");
    assert_eq!(
        outbound[1],
        json!({
            "urn": urn("stable.example.com", "Service", "gone-service"),
            "type": "depends-on",
            "unresolved": true
        })
    );
    assert!(
        !serde_json::to_string(&shaped)
            .expect("serialises")
            .contains("deleted"),
        "the tool never names a cause it cannot observe"
    );
}

/// A realistic shaped entry stays inside T3-D2's budget — the 13× that makes the tool small.
#[rstest]
fn test_a_shaped_entry_stays_inside_its_byte_budget() {
    let shaped = shape::group(
        &entries(vec![mock_entry(
            "outbound",
            "authentication-service",
            "depends-on",
            true,
        )]),
        Grouping::ByDirection,
        None,
    );

    let bytes = serde_json::to_string(&shaped["outbound"][0])
        .expect("serialises")
        .len();
    assert!(
        bytes <= MAX_SHAPED_ENTRY_BYTES,
        "a shaped entry is {bytes} B"
    );
}

// ---------------------------------------------------------------------------------------------
// T3-D1 — client-side grouping.
// ---------------------------------------------------------------------------------------------

/// A flat fixture spanning both directions and two types.
fn mock_flat() -> Vec<ItemRelationshipEntry> {
    entries(vec![
        mock_entry("outbound", "auth-service", "depends-on", true),
        mock_entry("inbound", "frontend", "depends-on", true),
        mock_entry("outbound", "billing", "part-of", true),
    ])
}

/// By direction, `type` is per entry and `direction` is the key; both keys are present.
#[rstest]
fn test_grouping_by_direction() {
    assert_eq!(
        shape::group(&mock_flat(), Grouping::ByDirection, None),
        json!({
            "outbound": [
                { "name": "auth-service", "kind": "Service", "type": "depends-on" },
                { "name": "billing", "kind": "Service", "type": "part-of" }
            ],
            "inbound": [
                { "name": "frontend", "kind": "Service", "type": "depends-on" }
            ]
        })
    );
}

/// By type, `direction` is per entry and `type` is the key — the same flat input, never both.
#[rstest]
fn test_grouping_by_type() {
    assert_eq!(
        shape::group(&mock_flat(), Grouping::ByType, None),
        json!({
            "depends-on": [
                { "name": "auth-service", "kind": "Service", "direction": "outbound" },
                { "name": "frontend", "kind": "Service", "direction": "inbound" }
            ],
            "part-of": [
                { "name": "billing", "kind": "Service", "direction": "outbound" }
            ]
        })
    );
}

/// An empty direction group is `[]`, not absent.
#[rstest]
fn test_an_empty_direction_group_is_an_empty_array() {
    let shaped = shape::group(
        &entries(vec![mock_entry(
            "outbound",
            "auth-service",
            "depends-on",
            true,
        )]),
        Grouping::ByDirection,
        None,
    );

    assert_eq!(shaped["inbound"], json!([]));
}

/// With a `direction` filter only that key appears: `inbound: []` would claim there are none
/// when none were asked for.
#[rstest]
fn test_a_direction_filter_shows_only_its_group() {
    let shaped = shape::group(
        &entries(vec![mock_entry(
            "outbound",
            "auth-service",
            "depends-on",
            true,
        )]),
        Grouping::ByDirection,
        Some(RelationshipDirection::Outbound),
    );

    assert!(shaped.get("inbound").is_none(), "{shaped}");
    assert_eq!(shaped["outbound"].as_array().map(Vec::len), Some(1));
}

// ---------------------------------------------------------------------------------------------
// Coordinate resolution and T3-D6.
// ---------------------------------------------------------------------------------------------

/// Without `kind`, a single probe row gives the address — and the item is read there.
#[rstest]
#[tokio::test]
async fn test_the_kindless_path_takes_the_address_from_the_probe() {
    let engine = MockEngine::start().await;
    mount(
        &engine,
        GLOBAL_PATH,
        200,
        mock_page(vec![mock_item(ITEM)], None),
        None,
    )
    .await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(vec![], None),
        None,
    )
    .await;

    let payload = run(&mock_context(&engine), mock_input(ITEM))
        .await
        .expect("the item is described");

    assert_eq!(
        (payload["group"].clone(), payload["family"].clone()),
        (json!("stable.example.com"), json!("services"))
    );
    let sent = requests(&engine).await;
    let probe = sent.iter().find(|(p, _)| p == GLOBAL_PATH).expect("probed");
    assert!(
        probe.1.contains("limit=2"),
        "two rows tell unique from several: {}",
        probe.1
    );
}

/// With `kind`, the core's point lookup gives the address — no name probe.
#[rstest]
#[tokio::test]
async fn test_the_kinded_path_resolves_from_the_type() {
    let engine = MockEngine::start().await;
    mount_happy_path(&engine).await;

    run(&mock_context(&engine), mock_kinded_input())
        .await
        .expect("the item is described");

    assert!(
        requests(&engine)
            .await
            .iter()
            .all(|(p, _)| p != GLOBAL_PATH),
        "no name probe when kind is given"
    );
}

/// Two matches are candidates, **never** a pick: no item is read at all.
#[rstest]
#[tokio::test]
async fn test_an_ambiguous_name_returns_candidates_not_a_pick() {
    let engine = MockEngine::start().await;
    let mut other = mock_item(ITEM);
    other["apiVersion"] = json!("example.com/v1");
    other["kind"] = json!("Template");
    other["metadata"]["family"] = json!("templates");
    mount(
        &engine,
        GLOBAL_PATH,
        200,
        mock_page(vec![mock_item(ITEM), other], None),
        None,
    )
    .await;

    let error = run(&mock_context(&engine), mock_input(ITEM))
        .await
        .expect_err("an ambiguous name is not guessed");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    let kinds: Vec<Value> = error
        .details
        .as_deref()
        .and_then(|details| details["candidates"].as_array().cloned())
        .expect("candidates")
        .into_iter()
        .map(|candidate| candidate["kind"].clone())
        .collect();
    assert_eq!(kinds, vec![json!("Service"), json!("Template")]);
    assert!(
        error
            .next_step
            .as_deref()
            .is_some_and(|step| step.contains("kind"))
    );
    let sent = requests(&engine).await;
    assert!(
        sent.iter().all(|(p, _)| p != ITEM_PATH),
        "no item was read: no silent pick"
    );
    assert!(
        sent.iter()
            .any(|(p, q)| p == GLOBAL_PATH
                && q.contains(&format!("limit={MAX_AMBIGUOUS_CANDIDATES}"))),
        "the wider fetch runs on the ambiguous branch"
    );
}

/// A name that matches nothing is `not_found`, with near matches when there are some.
#[rstest]
#[tokio::test]
async fn test_a_name_matching_nothing_offers_near_matches() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(GLOBAL_PATH))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![], None)))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(GLOBAL_PATH))
        .and(query_param("limit", "5"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_item(ITEM)], None)),
        )
        .mount(engine.server())
        .await;

    let error = run(&mock_context(&engine), mock_input("gateway"))
        .await
        .expect_err("nothing matches exactly");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"][0]["name"].clone()),
        Some(json!(ITEM))
    );
}

/// An item whose type no longer exists is `unaddressable_item`, never an empty answer (D30).
#[rstest]
#[tokio::test]
async fn test_a_null_family_is_unaddressable() {
    let engine = MockEngine::start().await;
    let mut orphan = mock_item(ITEM);
    orphan["metadata"]
        .as_object_mut()
        .expect("metadata")
        .remove("family");
    mount(
        &engine,
        GLOBAL_PATH,
        200,
        mock_page(vec![orphan], None),
        None,
    )
    .await;

    let error = run(&mock_context(&engine), mock_input(ITEM))
        .await
        .expect_err("an orphan cannot be addressed");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNADDRESSABLE_ITEM, Remedy::Escalate)
    );
}

/// An unknown `kind` is `not_found`, as T2-D9.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_is_not_found() {
    let engine = MockEngine::start().await;
    mount(&engine, TYPES_PATH, 200, mock_page(vec![], None), None).await;

    let error = run(
        &mock_context(&engine),
        DescribeItemInput {
            kind: Some("Nothing".to_string()),
            ..mock_input(ITEM)
        },
    )
    .await
    .expect_err("an unknown kind fails");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
}

// ---------------------------------------------------------------------------------------------
// T3-D5 — partial success, both directions.
// ---------------------------------------------------------------------------------------------

/// The relationships call failing leaves the item, `relationships: null` and a warning.
#[rstest]
#[case::unavailable(500)]
#[case::our_own_request(400)]
#[tokio::test]
async fn test_failed_relationships_still_return_the_item(#[case] status: u16) {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        status,
        mock_error_body(status, "boom"),
        None,
    )
    .await;

    let output = run_output(&mock_context(&engine), mock_kinded_input())
        .await
        .expect("a failed secondary fetch degrades the answer, it does not remove it");
    let rendered = output.render(None);

    assert_eq!(rendered["name"], json!(ITEM));
    assert!(
        rendered["relationships"].is_null(),
        "null, not {{}}: the fetch failed"
    );
    assert!(rendered.get("relationshipsTruncated").is_none());
    assert!(
        rendered["warnings"][0]
            .as_str()
            .is_some_and(|text| text.contains("relationships")),
        "{rendered}"
    );
}

/// The item call failing fails the whole call — there is nothing to describe.
#[rstest]
#[tokio::test]
async fn test_a_failed_item_fails_the_call() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 500, mock_error_body(500, "boom"), None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(vec![], None),
        None,
    )
    .await;

    let error = run(&mock_context(&engine), mock_kinded_input())
        .await
        .expect_err("without the item there is no answer");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
}

// ---------------------------------------------------------------------------------------------
// Truncation, the cursor, and the page size.
// ---------------------------------------------------------------------------------------------

/// More than a page sets `relationshipsTruncated` **and** returns a cursor; a last page, neither.
#[rstest]
#[case::more(Some("page-2"), true)]
#[case::last(None, false)]
#[tokio::test]
async fn test_truncation_comes_with_a_cursor(#[case] next: Option<&str>, #[case] truncated: bool) {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(
            vec![mock_entry("outbound", "auth-service", "depends-on", true)],
            next,
        ),
        None,
    )
    .await;

    let payload = run(&mock_context(&engine), mock_kinded_input())
        .await
        .expect("the item is described");

    assert_eq!(payload["relationshipsTruncated"], json!(truncated));
    assert_eq!(payload["relationshipCursor"].is_string(), truncated);
}

/// A cursor continues the relationships of the same item, without resolving it again.
#[rstest]
#[tokio::test]
async fn test_a_cursor_continues_without_resolving_again() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    Mock::given(method("GET"))
        .and(path(RELATIONSHIPS_PATH))
        .and(query_param_is_missing("continue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
            vec![mock_entry("outbound", "auth-service", "depends-on", true)],
            Some("page-2"),
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(RELATIONSHIPS_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
            vec![mock_entry("outbound", "billing", "part-of", true)],
            None,
        )))
        .mount(engine.server())
        .await;
    let context = mock_context(&engine);

    let first = run(&context, mock_kinded_input()).await.expect("page one");
    let second = run(
        &context,
        DescribeItemInput {
            relationship_cursor: first["relationshipCursor"].as_str().map(str::to_string),
            ..mock_kinded_input()
        },
    )
    .await
    .expect("page two");

    assert_eq!(
        second["relationships"]["outbound"][0]["name"],
        json!("billing")
    );
    let lookups = requests(&engine)
        .await
        .iter()
        .filter(|(p, _)| p == TYPES_PATH)
        .count();
    assert_eq!(lookups, 1, "the kind is resolved on the first page only");
}

/// A cursor that does not decode, or belongs to another direction, is `invalid_cursor`.
#[rstest]
#[tokio::test]
async fn test_a_bad_cursor_is_invalid() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(
            vec![mock_entry("outbound", "auth-service", "depends-on", true)],
            Some("page-2"),
        ),
        None,
    )
    .await;
    let context = mock_context(&engine);
    let first = run(&context, mock_kinded_input()).await.expect("page one");
    let minted = first["relationshipCursor"].as_str().map(str::to_string);

    for (cursor, direction) in [
        (Some("not-a-cursor".to_string()), None),
        (minted, Some(Direction::Inbound)),
    ] {
        let error = run(
            &context,
            DescribeItemInput {
                relationship_cursor: cursor,
                direction,
                ..mock_kinded_input()
            },
        )
        .await
        .expect_err("a bad cursor is an error");

        assert_eq!(
            (error.code, error.remedy),
            (codes::INVALID_CURSOR, Remedy::RetryAfterChange)
        );
    }
}

/// `relationship_limit` is clamped into the engine's range and echoed only when changed.
#[rstest]
#[case::default(None, "limit=50", None)]
#[case::above_the_maximum(Some(500), "limit=200", Some(200))]
#[case::zero(Some(0), "limit=1", Some(1))]
#[tokio::test]
async fn test_the_relationship_limit_is_clamped_and_echoed(
    #[case] requested: Option<u16>,
    #[case] sent: &str,
    #[case] echoed: Option<u64>,
) {
    let engine = MockEngine::start().await;
    mount_happy_path(&engine).await;

    let payload = run(
        &mock_context(&engine),
        DescribeItemInput {
            relationship_limit: requested,
            ..mock_kinded_input()
        },
    )
    .await
    .expect("the item is described");

    let sent_query = requests(&engine)
        .await
        .into_iter()
        .find(|(p, _)| p == RELATIONSHIPS_PATH)
        .map(|(_, query)| query)
        .expect("relationships were read");
    assert!(sent_query.contains(sent), "{sent_query}");
    assert_eq!(
        payload.get("relationshipLimit").and_then(Value::as_u64),
        echoed
    );
}

// ---------------------------------------------------------------------------------------------
// T3-D4 — concurrency, and T3-D1/T3-D7 on the wire.
// ---------------------------------------------------------------------------------------------

/// The item and its relationships are fetched **concurrently**: the pair takes about as long as
/// the slower one, not the sum.
#[rstest]
#[tokio::test]
async fn test_the_two_calls_are_concurrent() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), Some(ARM_DELAY)).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(vec![], None),
        Some(ARM_DELAY),
    )
    .await;
    let context = mock_context(&engine);

    let started = Instant::now();
    run(&context, mock_kinded_input())
        .await
        .expect("the item is described");
    let elapsed = started.elapsed();

    assert!(
        elapsed < CONCURRENT_CEILING,
        "took {elapsed:?}: two {ARM_DELAY:?} arms ran one after the other"
    );
}

/// Both groupings come from one `groupBy`-free call, and `rawq` is never sent to it.
#[rstest]
#[case::direction(GroupBy::Direction)]
#[case::relationship_type(GroupBy::Type)]
#[tokio::test]
async fn test_no_group_by_and_no_rawq_reach_the_engine(#[case] group_by: GroupBy) {
    let engine = MockEngine::start().await;
    mount_happy_path(&engine).await;

    run(
        &mock_context(&engine),
        DescribeItemInput {
            group_by: Some(group_by),
            ..mock_kinded_input()
        },
    )
    .await
    .expect("the item is described");

    let (_, query) = requests(&engine)
        .await
        .into_iter()
        .find(|(p, _)| p == RELATIONSHIPS_PATH)
        .expect("relationships were read");
    assert!(
        !query.contains("groupBy") && !query.contains("rawq"),
        "{query}"
    );
}

// ---------------------------------------------------------------------------------------------
// §5 — the output, and T3-D8's switches.
// ---------------------------------------------------------------------------------------------

/// The documented order, `customFields` included when the item has them.
#[rstest]
#[tokio::test]
async fn test_the_output_has_the_documented_shape() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    let mut item = mock_item(ITEM);
    item["customFields"] = json!({ "sensitivity": "high" });
    mount(&engine, ITEM_PATH, 200, item, None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(vec![], None),
        None,
    )
    .await;

    let payload = run(&mock_context(&engine), mock_kinded_input())
        .await
        .expect("the item is described");

    let keys: Vec<&str> = payload
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "name",
            "kind",
            "group",
            "version",
            "family",
            "title",
            "labels",
            "spec",
            "customFields",
            "relationships",
            "relationshipsTruncated"
        ]
    );
    assert_eq!(payload["customFields"], json!({ "sensitivity": "high" }));
}

/// `include_relationships: false` is **one** engine call; `include_spec: false` drops the spec.
#[rstest]
#[tokio::test]
async fn test_the_include_switches() {
    let engine = MockEngine::start().await;
    mount_happy_path(&engine).await;

    let payload = run(
        &mock_context(&engine),
        DescribeItemInput {
            include_spec: false,
            include_relationships: false,
            ..mock_kinded_input()
        },
    )
    .await
    .expect("the item is described");

    assert!(payload.get("spec").is_none());
    assert!(payload.get("relationships").is_none());
    assert!(
        requests(&engine)
            .await
            .iter()
            .all(|(p, _)| p != RELATIONSHIPS_PATH),
        "no second call"
    );
}

// ---------------------------------------------------------------------------------------------
// §7 — the rest of the table.
// ---------------------------------------------------------------------------------------------

/// Boundary bounds are `invalid_input` naming the parameter, before the engine is asked.
#[rstest]
#[case::empty_name(mock_input(""), "name")]
#[case::long_name(mock_input(&"n".repeat(MAX_NAME_BYTES + 1)), "name")]
#[case::bad_kind(DescribeItemInput { kind: Some("not a kind".to_string()), ..mock_input(ITEM) }, "kind")]
#[tokio::test]
async fn test_an_input_over_its_bound_is_refused(
    #[case] input: DescribeItemInput,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&mock_context(&engine), input)
        .await
        .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["field"].clone()),
        Some(json!(field))
    );
    assert!(requests(&engine).await.is_empty());
}

/// The call's own budget running out is `deadline_exceeded`.
#[rstest]
#[tokio::test]
async fn test_a_spent_deadline_is_deadline_exceeded() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(
        &engine,
        ITEM_PATH,
        200,
        mock_item(ITEM),
        Some(Duration::from_millis(500)),
    )
    .await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(vec![], None),
        None,
    )
    .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), Duration::from_millis(150)),
        mock_kinded_input(),
    )
    .await
    .expect_err("the deadline runs out first");

    assert_eq!(
        (error.code, error.remedy),
        (codes::DEADLINE_EXCEEDED, Remedy::Retry)
    );
}

/// Rule 5 — a caller that has gone away stops the fan-out.
#[rstest]
#[tokio::test]
async fn test_a_cancelled_call_reports_cancelled() {
    let engine = MockEngine::start().await;
    mount_happy_path(&engine).await;
    let context = mock_context(&engine);
    context.cancellation().cancel();

    let error = run(&context, mock_kinded_input())
        .await
        .expect_err("cancelled");

    assert_eq!(error.code, codes::CANCELLED);
}

// ---------------------------------------------------------------------------------------------
// §6 — the byte golden.
// ---------------------------------------------------------------------------------------------

/// An item with 50 relationships serialises to a recorded size — the shaper regressing is a diff.
#[rstest]
#[tokio::test]
async fn test_fifty_relationships_serialise_to_their_recorded_size() {
    let engine = MockEngine::start().await;
    mount_service_type(&engine).await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    let fifty = (0..50)
        .map(|index| {
            mock_entry(
                if index % 2 == 0 {
                    "outbound"
                } else {
                    "inbound"
                },
                &format!("service-{index:02}"),
                if index % 3 == 0 {
                    "part-of"
                } else {
                    "depends-on"
                },
                true,
            )
        })
        .collect();
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(fifty, Some("page-2")),
        None,
    )
    .await;

    let payload = run(&mock_context(&engine), mock_kinded_input())
        .await
        .expect("the item is described");
    let actual = serde_json::to_string(&payload).expect("serialises").len();

    let tolerance = RECORDED_FIFTY_RELATIONSHIPS_BYTES * SIZE_TOLERANCE_PERCENT / 100;
    assert!(
        actual.abs_diff(RECORDED_FIFTY_RELATIONSHIPS_BYTES) <= tolerance,
        "50 relationships serialise to {actual} B, recorded {RECORDED_FIFTY_RELATIONSHIPS_BYTES} B \
         (±{SIZE_TOLERANCE_PERCENT} %). Update the recording if the growth is intended."
    );
}

// ---------------------------------------------------------------------------------------------
// DR-80 — a kind is unique per group, not per tenant.
// ---------------------------------------------------------------------------------------------

/// A shared kind with no `group` is answered with its candidates; with `group`, the item is read in
/// the named group's family.
#[rstest]
#[tokio::test]
async fn test_a_shared_kind_needs_its_group() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.group=stable.example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
            vec![mock_item_type_definition(
                "Service",
                "services",
                "stable.example.com",
            )],
            None,
        )))
        .with_priority(1)
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
            vec![
                mock_item_type_definition("Service", "services", "stable.example.com"),
                mock_item_type_definition("Service", "services", "other.example.com"),
            ],
            None,
        )))
        .with_priority(2)
        .mount(engine.server())
        .await;
    mount(&engine, ITEM_PATH, 200, mock_item(ITEM), None).await;
    mount(
        &engine,
        RELATIONSHIPS_PATH,
        200,
        mock_page(vec![], None),
        None,
    )
    .await;
    let context = mock_context(&engine);

    let error = run(&context, mock_kinded_input())
        .await
        .expect_err("a shared kind is not guessed");
    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .and_then(|details| details["candidates"].as_array().map(Vec::len)),
        Some(2)
    );

    let payload = run(
        &context,
        DescribeItemInput {
            group: Some("stable.example.com".to_string()),
            ..mock_kinded_input()
        },
    )
    .await
    .expect("the pair names one type");
    assert_eq!(payload["group"], json!("stable.example.com"));
}

/// `group` without `kind` is refused before the engine is asked.
#[rstest]
#[tokio::test]
async fn test_a_group_without_a_kind_is_refused() {
    let engine = MockEngine::start().await;

    let error = run(
        &mock_context(&engine),
        DescribeItemInput {
            group: Some("stable.example.com".to_string()),
            ..mock_input(ITEM)
        },
    )
    .await
    .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert!(requests(&engine).await.is_empty());
}
