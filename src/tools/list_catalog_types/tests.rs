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
    tools::list_catalog_types::{
        CatalogType, ListCatalogTypes, ListCatalogTypesInput, MAX_SEARCH_BYTES,
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    pagination::MAX_INTERNAL_PAGES,
    testing::{MockEngine, mock_acl_context, mock_error_body},
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{header, method, path, query_param, query_param_is_missing},
};

/// Where the type listing lives, relative to the mock engine's root.
const LISTING_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// A port nothing listens on, for the unreachable-engine row.
const UNREACHABLE_ENGINE: &str = "http://127.0.0.1:9";

/// The per-call budget the fixtures run under, matching the shipped default.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// A description exercising everything §7 forbids touching: markdown, several paragraphs, a code
/// fence and non-ASCII text. It must come back **byte for byte**.
const RICH_DESCRIPTION: &str = "# Services\n\nA **deployable** unit — «données» and Überwachung      included.\n\nValid tiers:\n\n- `gold`\n- `silver`\n\n```yaml\ntier: gold\n```\n\nTrailing      paragraph, after the example.";

/// The recorded size of the realistic fixture's output (T1 §8, core §12.4). **Regression
/// detection, not a limit**: update it deliberately when growth is intended.
const RECORDED_REALISTIC_BYTES: usize = 30_469;

/// How far the realistic fixture may drift before the golden fails.
const SIZE_TOLERANCE_PERCENT: usize = 1;

/// How many types the realistic fixture carries — the seeded catalogue's count.
const REALISTIC_TYPE_COUNT: usize = 68;

/// One Item Type Definition as the engine returns it, with a schema large enough that the lean
/// model has something to skip. `extra` is merged into `spec`.
fn mock_itd(kind: &str, plural: &str, group: &str, extra: Value) -> Value {
    let mut spec = json!({
        "group": group,
        "names": { "kind": kind, "plural": plural },
        "scope": "Tenant",
        "versions": [{
            "name": "v1",
            "served": true,
            "schema": { "openAPIV31Schema": {
                "type": "object",
                "properties": { "spec": { "type": "object", "properties": {
                    "name": { "type": "string" },
                    "tier": { "type": "string", "enum": ["gold", "silver"] }
                } } }
            } }
        }],
    });

    if let (Some(spec), Value::Object(extra)) = (spec.as_object_mut(), extra) {
        spec.extend(extra);
    }

    json!({
        "apiVersion": "mia-platform.eu/v1",
        "kind": "ItemTypeDefinition",
        "metadata": { "name": format!("{plural}.{group}"), "description": "Not a briefing." },
        "spec": spec,
    })
}

/// A `List` envelope, with a continuation token when there is a next page.
fn mock_page(items: Vec<Value>, next: Option<&str>) -> Value {
    let mut metadata = json!({});
    if let Some(next) = next {
        metadata["continue"] = json!(next);
    }

    json!({ "apiVersion": "v1", "kind": "List", "metadata": metadata, "items": items })
}

/// A URL-safe ACL context for a fictional tenant.
fn mock_acl_for(tenant: &str) -> String {
    URL_SAFE_NO_PAD.encode(json!({ "organization": "my-org", "tenant": tenant }).to_string())
}

/// A call context against `base_url`, carrying `acl` and the given budget.
fn mock_context_at(base_url: &str, acl: &str, budget: Duration) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(acl),
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

/// A call context pointed at `engine`, as the default tenant.
fn mock_context(engine: &MockEngine) -> CallContext {
    mock_context_at(&engine.server().uri(), &mock_acl_context(), CALL_BUDGET)
}

/// Runs the tool against a mock serving `types` in one page, returning the payload.
async fn call_with(types: Vec<Value>, search: Option<&str>) -> Result<Value, ToolError> {
    let engine = MockEngine::start().await;
    engine.get_ok(LISTING_PATH, mock_page(types, None)).await;

    run(&mock_context(&engine), search).await
}

/// Runs the tool in `context` and returns the rendered payload.
async fn run(context: &CallContext, search: Option<&str>) -> Result<Value, ToolError> {
    ListCatalogTypes
        .call(
            context,
            ListCatalogTypesInput {
                search: search.map(str::to_string),
            },
        )
        .await
        .map(|output| output.payload().clone())
}

/// The one row a single-type listing produces.
async fn only_row(itd: Value) -> Value {
    let payload = call_with(vec![itd], None)
        .await
        .expect("the listing succeeds");
    assert_eq!(payload["total"], json!(1), "{payload}");

    payload["types"][0].clone()
}

/// The `kind`s of a payload's rows, in order.
fn kinds(payload: &Value) -> Vec<String> {
    payload["types"]
        .as_array()
        .expect("types is an array")
        .iter()
        .map(|row| row["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// §7 — `llmDescription` is returned verbatim.
// ---------------------------------------------------------------------------------------------

/// The description is **byte-identical** to the engine's: markdown, paragraphs, a code fence
/// and non-ASCII text all survive untouched.
#[rstest]
#[tokio::test]
async fn test_a_description_is_returned_byte_identical() {
    let row = only_row(mock_itd(
        "Service",
        "services",
        "mia-platform.eu",
        json!({ "llmDescription": RICH_DESCRIPTION }),
    ))
    .await;

    assert_eq!(row["description"].as_str(), Some(RICH_DESCRIPTION));
}

/// The clearest statement of why §7 exists: an opening negation, which any shortening risks
/// turning into its opposite, survives intact.
#[rstest]
#[tokio::test]
async fn test_a_description_opening_with_a_negation_survives() {
    let negation = "This type is not used for deployments. It records what was deployed, after \
                    the fact, for audit.";
    let row = only_row(mock_itd(
        "Release",
        "releases",
        "mia-platform.eu",
        json!({ "llmDescription": negation }),
    ))
    .await;

    assert_eq!(row["description"].as_str(), Some(negation));
}

/// A blank description is absent — omitted, never `""`.
#[rstest]
#[case::empty("")]
#[case::whitespace("   \n\t ")]
#[tokio::test]
async fn test_a_blank_description_is_omitted(#[case] blank: &str) {
    let row = only_row(mock_itd(
        "Service",
        "services",
        "mia-platform.eu",
        json!({ "llmDescription": blank }),
    ))
    .await;

    assert!(row.get("description").is_none(), "{row}");
}

/// Nothing is synthesised: with no `llmDescription`, neither `metadata.description` nor the
/// display name is backfilled into the briefing.
#[rstest]
#[tokio::test]
async fn test_nothing_is_synthesised_into_a_missing_description() {
    let row = only_row(mock_itd(
        "Service",
        "services",
        "mia-platform.eu",
        json!({ "names": { "kind": "Service", "plural": "services", "displayPlural": "Services" } }),
    ))
    .await;

    assert!(row.get("description").is_none(), "{row}");
    assert_eq!(row["displayName"], json!("Services"));
}

// ---------------------------------------------------------------------------------------------
// §6 — `search`.
// ---------------------------------------------------------------------------------------------

/// Three types, each findable by exactly one field.
fn mock_searchable() -> Vec<Value> {
    vec![
        mock_itd("Service", "services", "mia-platform.eu", json!({})),
        mock_itd(
            "Template",
            "templates",
            "mia-platform.eu",
            json!({ "names": { "kind": "Template", "plural": "templates",
                               "displayPlural": "Blueprints" } }),
        ),
        mock_itd(
            "Monitor",
            "monitors",
            "example.com",
            json!({ "llmDescription": "Tracks Überwachung signals." }),
        ),
    ]
}

/// Matching is over all four fields: `kind`, `family`, `displayName`, `llmDescription`.
#[rstest]
#[case::kind("Service", "Service")]
#[case::family("templates", "Template")]
#[case::display_name("blueprint", "Template")]
#[case::description("signals", "Monitor")]
#[tokio::test]
async fn test_search_matches_each_field(#[case] term: &str, #[case] expected: &str) {
    let payload = call_with(mock_searchable(), Some(term))
        .await
        .expect("the listing succeeds");

    assert_eq!(kinds(&payload), vec![expected.to_string()]);
}

/// Case-insensitive under full Unicode lowercasing, not ASCII-only.
#[rstest]
#[case::ascii("SERVICE", "Service")]
#[case::non_ascii("ÜBERWACHUNG", "Monitor")]
#[tokio::test]
async fn test_search_is_case_insensitive(#[case] term: &str, #[case] expected: &str) {
    let payload = call_with(mock_searchable(), Some(term))
        .await
        .expect("the listing succeeds");

    assert_eq!(kinds(&payload), vec![expected.to_string()]);
}

/// T1-D5's whole point: a term present only in the **tail** of a long description still finds
/// its type, because nothing was shortened.
#[rstest]
#[tokio::test]
async fn test_search_finds_a_term_only_in_the_descriptions_tail() {
    let description = format!(
        "{} The last sentence mentions quarantine.",
        "Filler. ".repeat(400)
    );
    let payload = call_with(
        vec![
            mock_itd(
                "Service",
                "services",
                "mia-platform.eu",
                json!({ "llmDescription": description }),
            ),
            mock_itd("Template", "templates", "mia-platform.eu", json!({})),
        ],
        Some("quarantine"),
    )
    .await
    .expect("the listing succeeds");

    assert_eq!(kinds(&payload), vec!["Service".to_string()]);
}

/// T1-D9 — no match is **not** an empty catalogue: the term and the unfiltered count come back.
#[rstest]
#[tokio::test]
async fn test_no_match_reports_the_term_and_what_it_filtered() {
    let payload = call_with(mock_searchable(), Some("gatewy"))
        .await
        .expect("an empty match is not an error");

    assert_eq!(
        payload,
        json!({ "types": [], "total": 0, "search": "gatewy", "filteredFrom": 3 })
    );
}

/// Without a search the two keys are absent, so an empty catalogue reads as one.
#[rstest]
#[tokio::test]
async fn test_no_types_visible_is_an_empty_list_not_an_error() {
    let payload = call_with(vec![], None)
        .await
        .expect("an empty catalogue is not an error");

    assert_eq!(payload, json!({ "types": [], "total": 0 }));
}

/// NFR-10 — an over-long term is refused **before** the engine is asked anything; the limit
/// itself is accepted.
#[rstest]
#[tokio::test]
async fn test_an_over_long_search_is_rejected_before_the_engine() {
    let engine = MockEngine::start().await;
    engine.get_ok(LISTING_PATH, mock_page(vec![], None)).await;
    let context = mock_context(&engine);

    let error = run(&context, Some(&"x".repeat(MAX_SEARCH_BYTES + 1)))
        .await
        .expect_err("an over-long search is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error
            .details
            .as_ref()
            .map(|details| details["field"].clone()),
        Some(json!("search"))
    );
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the engine was called for a request that was already invalid"
    );

    run(&context, Some(&"x".repeat(MAX_SEARCH_BYTES)))
        .await
        .expect("a term of exactly the limit is accepted");
}

// ---------------------------------------------------------------------------------------------
// T1-D4 — version selection, delegated to the core.
// ---------------------------------------------------------------------------------------------

/// A type with no served version is omitted: nothing about it is addressable.
#[rstest]
#[tokio::test]
async fn test_a_type_with_no_served_version_is_omitted() {
    let payload = call_with(
        vec![
            mock_itd("Service", "services", "mia-platform.eu", json!({})),
            mock_itd(
                "Retired",
                "retireds",
                "mia-platform.eu",
                json!({ "versions": [{ "name": "v1", "served": false }] }),
            ),
        ],
        None,
    )
    .await
    .expect("the listing succeeds");

    assert_eq!(kinds(&payload), vec!["Service".to_string()]);
    assert_eq!(payload["total"], json!(1));
}

/// The version the core's rule selects is the one that reaches the row.
#[rstest]
#[tokio::test]
async fn test_the_selected_version_reaches_the_row() {
    let row = only_row(mock_itd(
        "Service",
        "services",
        "mia-platform.eu",
        json!({ "versions": [
            { "name": "v1", "served": true, "deprecated": true },
            { "name": "v2", "served": true },
            { "name": "v3alpha1", "served": false }
        ] }),
    ))
    .await;

    assert_eq!(row["version"], json!("v2"));
}

// ---------------------------------------------------------------------------------------------
// §4 — projection and ordering.
// ---------------------------------------------------------------------------------------------

/// The row is exactly §4's shape, with absent fields omitted rather than null.
///
/// Compared as a value, not as a string: the payload is a `serde_json::Value`, whose keys
/// serialise alphabetically, so §4's *"field order is the serialised order"* does not hold today
/// for this or any tool. Recorded as an open point rather than asserted either way.
#[rstest]
#[tokio::test]
async fn test_a_row_is_the_documented_shape() {
    let row = only_row(mock_itd(
        "Service",
        "services",
        "mia-platform.eu",
        json!({
            "names": { "kind": "Service", "plural": "services", "displayPlural": "Services" },
            "llmDescription": "A deployable unit.",
            "history": { "enabled": true }
        }),
    ))
    .await;

    assert_eq!(
        row,
        json!({
            "kind": "Service",
            "family": "services",
            "group": "mia-platform.eu",
            "version": "v1",
            "displayName": "Services",
            "description": "A deployable unit.",
            "historyEnabled": true
        })
    );
}

/// `spec.history` absent means history is off, and `displayPlural` absent omits the field.
#[rstest]
#[tokio::test]
async fn test_absent_history_is_false_and_absent_display_name_is_omitted() {
    let row = only_row(mock_itd(
        "Service",
        "services",
        "mia-platform.eu",
        json!({}),
    ))
    .await;

    assert_eq!(row["historyEnabled"], json!(false));
    assert!(row.get("displayName").is_none(), "{row}");
}

/// T1-D8 — ordered by `kind` byte-wise whatever the engine's order, with `group` breaking a tie
/// two groups sharing a kind would otherwise leave to chance.
#[rstest]
#[tokio::test]
async fn test_rows_are_ordered_by_kind_then_group() {
    let payload = call_with(
        vec![
            mock_itd("Template", "templates", "mia-platform.eu", json!({})),
            mock_itd("Service", "services", "zeta.example.com", json!({})),
            mock_itd("Api", "apis", "mia-platform.eu", json!({})),
            mock_itd("Service", "services", "alpha.example.com", json!({})),
        ],
        None,
    )
    .await
    .expect("the listing succeeds");

    let order: Vec<(String, String)> = payload["types"]
        .as_array()
        .expect("types is an array")
        .iter()
        .map(|row| {
            (
                row["kind"].as_str().unwrap_or_default().to_string(),
                row["group"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    assert_eq!(
        order,
        vec![
            ("Api".to_string(), "mia-platform.eu".to_string()),
            ("Service".to_string(), "alpha.example.com".to_string()),
            ("Service".to_string(), "zeta.example.com".to_string()),
            ("Template".to_string(), "mia-platform.eu".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// §9 — every row, asserting `code` **and** `remedy`.
// ---------------------------------------------------------------------------------------------

/// Each engine status T1 can meet, mapped onto the closed set. A `400` is `server_defect`
/// because T1 sends nothing of the caller's: the model must not be told to change its input.
#[rstest]
#[case::server_error(500, codes::CATALOG_UNAVAILABLE, Remedy::Retry)]
#[case::unavailable(503, codes::CATALOG_UNAVAILABLE, Remedy::Retry)]
#[case::bad_request(400, codes::SERVER_DEFECT, Remedy::Escalate)]
#[case::not_acceptable(406, codes::SERVER_DEFECT, Remedy::Escalate)]
#[case::unauthenticated(401, codes::UNAUTHENTICATED, Remedy::Escalate)]
#[case::forbidden(403, codes::FORBIDDEN, Remedy::Escalate)]
#[tokio::test]
async fn test_engine_statuses_map_to_their_documented_rows(
    #[case] status: u16,
    #[case] code: &str,
    #[case] remedy: Remedy,
) {
    let engine = MockEngine::start().await;
    engine.get_error(LISTING_PATH, status, "refused").await;

    let error = run(&mock_context(&engine), None)
        .await
        .expect_err("an engine error fails the call");

    assert_eq!((error.code, error.remedy), (code, remedy));
}

/// An engine nobody answers for is unavailable — never an empty catalogue.
#[rstest]
#[tokio::test]
async fn test_an_unreachable_engine_is_catalog_unavailable() {
    let context = mock_context_at(UNREACHABLE_ENGINE, &mock_acl_context(), CALL_BUDGET);

    let error = run(&context, None)
        .await
        .expect_err("an unreachable engine fails the call");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
}

/// The call's own budget running out is `deadline_exceeded`, not the catalog failing.
#[rstest]
#[tokio::test]
async fn test_a_spent_deadline_is_deadline_exceeded() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(LISTING_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_page(vec![], None))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(engine.server())
        .await;
    let context = mock_context_at(
        &engine.server().uri(),
        &mock_acl_context(),
        Duration::from_millis(100),
    );

    let error = run(&context, None)
        .await
        .expect_err("the deadline runs out first");

    assert_eq!(
        (error.code, error.remedy),
        (codes::DEADLINE_EXCEEDED, Remedy::Retry)
    );
}

/// Rule 5 — a caller that has gone away stops the walk, and the call reports `cancelled`.
#[rstest]
#[tokio::test]
async fn test_a_cancelled_call_stops_and_reports_cancelled() {
    let engine = MockEngine::start().await;
    engine.get_ok(LISTING_PATH, mock_page(vec![], None)).await;
    let context = mock_context(&engine);
    context.cancellation().cancel();

    let error = run(&context, None)
        .await
        .expect_err("a cancelled call does not answer");

    assert_eq!(error.code, codes::CANCELLED);
}

// ---------------------------------------------------------------------------------------------
// T1-D7 — pagination: all or nothing.
// ---------------------------------------------------------------------------------------------

/// `count` distinct types, `Kind0000`…, so a page boundary can be seen in the result.
fn mock_many(count: usize, offset: usize) -> Vec<Value> {
    (offset..offset + count)
        .map(|index| {
            mock_itd(
                &format!("Kind{index:04}"),
                &format!("kind{index:04}s"),
                "example.com",
                json!({}),
            )
        })
        .collect()
}

/// Serves `first` without a cursor and `second` once asked to continue from `page-2`.
async fn mount_two_pages(engine: &MockEngine, first: Value, second: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(LISTING_PATH))
        .and(query_param_is_missing("continue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(first))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(LISTING_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(second)
        .mount(engine.server())
        .await;
}

/// More than 200 types come back as **one** complete list.
#[rstest]
#[tokio::test]
async fn test_more_than_one_page_is_one_complete_list() {
    let engine = MockEngine::start().await;
    mount_two_pages(
        &engine,
        mock_page(mock_many(200, 0), Some("page-2")),
        ResponseTemplate::new(200).set_body_json(mock_page(mock_many(5, 200), None)),
    )
    .await;

    let payload = run(&mock_context(&engine), None)
        .await
        .expect("the listing succeeds");

    assert_eq!(payload["total"], json!(205));
    assert_eq!(kinds(&payload).last().map(String::as_str), Some("Kind0204"));
}

/// A failing second page fails the whole call: a silently short list would make real types
/// look nonexistent.
#[rstest]
#[tokio::test]
async fn test_a_failing_second_page_fails_the_call() {
    let engine = MockEngine::start().await;
    mount_two_pages(
        &engine,
        mock_page(mock_many(200, 0), Some("page-2")),
        ResponseTemplate::new(500).set_body_json(mock_error_body(500, "Something went wrong")),
    )
    .await;

    let error = run(&mock_context(&engine), None)
        .await
        .expect_err("a partial list is never returned");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
}

/// `MAX_INTERNAL_PAGES` bounds an engine that never stops handing out cursors.
#[rstest]
#[tokio::test]
async fn test_the_internal_page_cap_is_enforced() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(LISTING_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(mock_many(1, 0), Some("again"))),
        )
        .mount(engine.server())
        .await;

    let error = run(&mock_context(&engine), None)
        .await
        .expect_err("an endless listing is not an answer");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert_eq!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .len(),
        MAX_INTERNAL_PAGES
    );
}

// ---------------------------------------------------------------------------------------------
// NFR-01 — tenancy, asserted although trivially true with no cache.
// ---------------------------------------------------------------------------------------------

/// Two tenants see their own catalogues. Trivially true today; asserted because it must survive
/// a future cache (T1 §14).
#[rstest]
#[tokio::test]
async fn test_two_tenants_see_their_own_catalogues() {
    let engine = MockEngine::start().await;
    for (tenant, kind) in [("tenant-one", "Service"), ("tenant-two", "Template")] {
        Mock::given(method("GET"))
            .and(path(LISTING_PATH))
            .and(header("x-mia-acl-context", mock_acl_for(tenant).as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(
                vec![mock_itd(kind, "things", "example.com", json!({}))],
                None,
            )))
            .mount(engine.server())
            .await;
    }

    for (tenant, kind) in [("tenant-one", "Service"), ("tenant-two", "Template")] {
        let context = mock_context_at(&engine.server().uri(), &mock_acl_for(tenant), CALL_BUDGET);
        let payload = run(&context, None).await.expect("the listing succeeds");

        assert_eq!(kinds(&payload), vec![kind.to_string()], "{tenant}");
    }
}

// ---------------------------------------------------------------------------------------------
// §8 — the byte golden: regression detection, not a limit.
// ---------------------------------------------------------------------------------------------

/// Sixty-eight realistic types, each with a ~300-byte briefing, serialise to a recorded size.
/// A projection regression shows up here as a diff while it is still a design discussion.
#[rstest]
#[tokio::test]
async fn test_the_realistic_catalogue_serialises_to_its_recorded_size() {
    let briefing = "Describes one deployable workload: its runtime, its owner and the environments \
                    it runs in. Use it to answer where something runs and who to ask about it. It \
                    is not a deployment record — releases are a separate type. Tiers are gold, \
                    silver and bronze; anything else is rejected by the schema.";
    let types = (0..REALISTIC_TYPE_COUNT)
        .map(|index| {
            let kind = format!("Workload{index:02}");
            let plural = format!("workload{index:02}s");
            mock_itd(
                &kind,
                &plural,
                "mia-platform.eu",
                json!({
                    "names": { "kind": kind, "plural": plural,
                               "displayPlural": format!("Workloads {index:02}") },
                    "llmDescription": briefing,
                    "history": { "enabled": index % 4 == 0 }
                }),
            )
        })
        .collect();

    let payload = call_with(types, None).await.expect("the listing succeeds");
    let actual = serde_json::to_string(&payload)
        .expect("the payload serialises")
        .len();

    let tolerance = RECORDED_REALISTIC_BYTES * SIZE_TOLERANCE_PERCENT / 100;
    assert!(
        actual.abs_diff(RECORDED_REALISTIC_BYTES) <= tolerance,
        "the realistic catalogue serialises to {actual} B, recorded {RECORDED_REALISTIC_BYTES} B \
         (±{SIZE_TOLERANCE_PERCENT} %). Update the recording if the growth is intended."
    );
}

/// The row type stays a plain value: no field serialises as `null`.
#[rstest]
fn test_absent_row_fields_never_serialise_as_null() {
    let row = CatalogType {
        kind: "Service".to_string(),
        family: "services".to_string(),
        group: "mia-platform.eu".to_string(),
        version: "v1".to_string(),
        display_name: None,
        description: None,
        history_enabled: false,
    };

    let rendered = serde_json::to_string(&row).expect("a row serialises");

    assert!(!rendered.contains("null"), "{rendered}");
}
