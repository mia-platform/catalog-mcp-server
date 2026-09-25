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
    address::ItemAddress,
    client::EngineClient,
    error::codes,
    identity::{ACL_CONTEXT_HEADER, AUTHORIZATION_HEADER, PRINCIPAL_ID_HEADER},
    models::ItemTypeDefinition,
    ops::{ListQuery, OPERATIONS},
    pagination::{EngineCursor, ListPage, paginate_all},
    testing::{
        MOCK_BEARER, MOCK_ITEM_NAME, MOCK_PRINCIPAL_ID, MockEngine, mock_acl_context,
        mock_identity, mock_identity_acl_only, mock_identity_empty, mock_item,
        mock_item_type_definition, mock_list_envelope,
    },
};
use rstest::rstest;
use std::sync::Arc;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, path_regex},
};

/// The address the fixtures use.
fn mock_address() -> ItemAddress {
    ItemAddress::new("stable.example.com", "v1", "services", MOCK_ITEM_NAME)
        .expect("a well-formed fixture address")
}

/// Calls every operation once against a mock that accepts anything, and returns the requests it
/// recorded.
///
/// This is what makes the NFR-11 assertion **structural**: it is driven by [`OPERATIONS`], so an
/// operation added without the identity pair fails here rather than being noticed in
/// production.
async fn record_every_operation(client: &EngineClient) -> Vec<wiremock::Request> {
    let query = ListQuery::default();

    // Each call is allowed to fail: what is under test is what went out, not what came back.
    let _ = client.list_items(&query).await;
    let _ = client.list_items_partial(&query).await;
    let _ = client.get_item(&mock_address()).await;
    let _ = client
        .list_item_type_definitions::<ItemTypeDefinition>(&query)
        .await;
    let family = mock_family();
    let _ = client.list_family_items_partial(&family, &query).await;
    let _ = client.count_items(&query).await;
    let _ = client.count_family_items(&family, &query).await;

    Vec::new()
}

/// The family every family-scoped operation is exercised against.
fn mock_family() -> crate::address::FamilyAddress {
    crate::address::FamilyAddress::new("stable.example.com", "v1", "services")
        .expect("a well-formed family")
}

/// Mounts a catch-all `200` so every operation gets an answer it can parse.
async fn mount_catch_all(engine: &MockEngine) {
    Mock::given(method("GET"))
        .and(path_regex(r".*item-type-definitions$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition(
                "Service",
                "services",
                "stable.example.com",
            )],
            None,
        )))
        .mount(engine.server())
        .await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![mock_item(MOCK_ITEM_NAME)], None)),
        )
        .mount(engine.server())
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/stable\.example\.com/v1/items/services/.*$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
}

// ---------------------------------------------------------------------------------------------
// NFR-11 — the propagation guarantee, as three tests rather than a convention (§7.4).
// ---------------------------------------------------------------------------------------------

/// (a) **Every** method in `ops` sends `x-mia-acl-context` *and* `x-mia-principal-id` when both
/// arrived — driven by the [`OPERATIONS`] list, so none can be skipped.
#[rstest]
#[tokio::test]
async fn test_every_operation_forwards_the_identity_pair() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let _ = record_every_operation(&engine.client(mock_identity())).await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    // Seven calls over six operations: the global listing has two projections.
    assert_eq!(
        requests.len(),
        7,
        "every operation in `ops` must be exercised here; `OPERATIONS` lists {}",
        OPERATIONS.len()
    );

    for request in &requests {
        assert_eq!(
            request
                .headers
                .get(ACL_CONTEXT_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(mock_acl_context().as_str()),
            "{} did not forward the ACL context",
            request.url.path()
        );
        assert_eq!(
            request
                .headers
                .get(PRINCIPAL_ID_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(MOCK_PRINCIPAL_ID),
            "{} did not forward the principal id",
            request.url.path()
        );
    }
}

/// The operation list and the exercised set must not drift apart: this is the assertion that
/// makes the test above meaningful when a wave adds an operation.
#[rstest]
fn test_the_operation_list_matches_what_the_client_implements() {
    let ids: Vec<&str> = OPERATIONS.iter().map(|spec| spec.id).collect();

    assert_eq!(
        ids,
        vec![
            "list_items",
            "get_item",
            "list_item_type_definitions",
            "list_family_items",
            "count_items",
            "count_family_items"
        ]
    );
}

/// (b) The bytes forwarded are byte-identical to the bytes received — including a `tenantName`
/// that survives, because the header is carried and never re-encoded from a parsed value.
#[rstest]
#[tokio::test]
async fn test_the_forwarded_bytes_equal_the_received_bytes() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let received = URL_SAFE_NO_PAD
        .encode(r#"{"organization":"my-org","tenant":"my-tenant","tenantName":"My Tenant"}"#);

    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let identity = Arc::new(crate::identity::CallerIdentity::new(
        Some(&received),
        Some(MOCK_PRINCIPAL_ID),
        Some(MOCK_BEARER),
        Some("test-request-0001"),
    ));

    let _ = engine
        .client(identity)
        .list_items(&ListQuery::default())
        .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert_eq!(
        requests[0]
            .headers
            .get(ACL_CONTEXT_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(received.as_str())
    );
}

/// (c) Neither header is ever synthesised: with none inbound, none goes out, and no default
/// appears.
#[rstest]
#[tokio::test]
async fn test_nothing_is_synthesised_when_no_identity_arrives() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let _ = engine
        .client(mock_identity_empty())
        .list_items(&ListQuery::default())
        .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert!(requests[0].headers.get(ACL_CONTEXT_HEADER).is_none());
    assert!(requests[0].headers.get(PRINCIPAL_ID_HEADER).is_none());
    assert!(requests[0].headers.get(AUTHORIZATION_HEADER).is_none());
}

/// **D47, asserted negatively.** A request with no identity at all still produces an engine
/// call: the server returns no `401` and raises no error of its own. This is the test that fails
/// if somebody re-adds a gate.
#[rstest]
#[tokio::test]
async fn test_a_request_with_no_identity_still_reaches_the_engine() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let response = engine
        .client(mock_identity_empty())
        .list_items(&ListQuery::default())
        .await
        .expect("no identity is not an error of ours");

    assert_eq!(response.value.items.len(), 1);
    assert_eq!(
        engine
            .server()
            .received_requests()
            .await
            .expect("the mock records its requests")
            .len(),
        1
    );
}

/// The in-cluster path forwards an ACL context and nothing else, and that is not our problem to
/// refuse either — the policy-guarded route will answer for it.
#[rstest]
#[tokio::test]
async fn test_an_acl_only_identity_is_forwarded_as_it_arrived() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let _ = engine
        .client(mock_identity_acl_only())
        .list_items(&ListQuery::default())
        .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert!(requests[0].headers.get(ACL_CONTEXT_HEADER).is_some());
    assert!(requests[0].headers.get(PRINCIPAL_ID_HEADER).is_none());
    assert!(requests[0].headers.get(AUTHORIZATION_HEADER).is_none());
}

/// D26 — `acl-filter` is never sent. It is policy-injected, and a second occurrence is a `400`.
#[rstest]
#[tokio::test]
async fn test_the_acl_filter_parameter_is_never_sent() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let _ = record_every_operation(&engine.client(mock_identity())).await;

    for request in engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
    {
        assert!(
            !request
                .url
                .query_pairs()
                .any(|(key, _)| key == "acl-filter"),
            "{} sent acl-filter",
            request.url
        );
    }
}

/// D26 — `x-jwt-payload` is dropped: it carries unverified claims by construction, and a field
/// that exists on one of two ingress paths becomes load-bearing by accident.
#[rstest]
#[tokio::test]
async fn test_the_jwt_payload_header_is_never_forwarded() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let _ = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert!(requests[0].headers.get("x-jwt-payload").is_none());
}

// ---------------------------------------------------------------------------------------------
// The operations themselves.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_get_item_addresses_the_right_path() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let item = engine
        .client(mock_identity())
        .get_item(&mock_address())
        .await
        .expect("the read succeeds");

    assert_eq!(item.value.metadata.name, MOCK_ITEM_NAME);

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert_eq!(
        requests[0].url.path(),
        "/stable.example.com/v1/items/services/example-item"
    );
}

#[rstest]
#[tokio::test]
async fn test_the_projection_sets_the_accept_header() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let client = engine.client(mock_identity());
    let _ = client.list_items(&ListQuery::default()).await;
    let _ = client.list_items_partial(&ListQuery::default()).await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert_eq!(
        requests[0]
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        requests[1]
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok()),
        Some("application/json;as=PartialObjectMetadata")
    );
}

#[rstest]
#[tokio::test]
async fn test_the_list_query_is_applied_to_the_url() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let query = ListQuery {
        limit: Some(25),
        cursor: Some(EngineCursor::new("engine-token-2")),
        raw_query: vec!["cmF3LW9uZQ".to_string(), "cmF3LXR3bw".to_string()],
        sort: vec!["metadata.name".to_string()],
        ..ListQuery::default()
    };

    let _ = engine.client(mock_identity()).list_items(&query).await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    let pairs: Vec<(String, String)> = requests[0]
        .url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    assert!(pairs.contains(&("limit".to_string(), "25".to_string())));
    assert!(pairs.contains(&("continue".to_string(), "engine-token-2".to_string())));
    assert!(pairs.contains(&("sort".to_string(), "metadata.name".to_string())));
    // `rawq` is repeatable and the engine AND-s every instance.
    assert_eq!(
        pairs.iter().filter(|(key, _)| key == "rawq").count(),
        2,
        "both rawq parameters must survive"
    );
}

/// A query parameter an endpoint does not declare is not sent to it: the engine answers an
/// undeclared parameter with a `400` the model could only misread.
#[rstest]
#[tokio::test]
async fn test_a_parameter_the_endpoint_does_not_declare_is_not_sent() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let query = ListQuery {
        field: vec!["spec.names.kind=Service".to_string()],
        ..ListQuery::default()
    };

    let client = engine.client(mock_identity());
    // `/items` does not declare `field`; the item-type-definition listing does.
    let _ = client.list_items(&query).await;
    let _ = client
        .list_item_type_definitions::<ItemTypeDefinition>(&query)
        .await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert!(!requests[0].url.query_pairs().any(|(key, _)| key == "field"));
    assert!(requests[1].url.query_pairs().any(|(key, _)| key == "field"));
}

#[rstest]
#[tokio::test]
async fn test_item_type_definitions_are_shaped() {
    let engine = MockEngine::start().await;
    mount_catch_all(&engine).await;

    let response = engine
        .client(mock_identity())
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery::default())
        .await
        .expect("the listing succeeds");

    let itd = &response.value.items[0];

    assert_eq!(itd.spec.names.kind, "Service");
    assert_eq!(itd.spec.names.plural, "services");
    assert_eq!(itd.spec.versions[0].name, "v1");
    assert!(itd.spec.versions[0].served);
    assert!(itd.spec.history_enabled());
}

/// An item whose type no longer exists is unaddressable — a real engine state, reported as
/// such, never as an empty result (D30).
#[rstest]
#[tokio::test]
async fn test_an_item_without_a_family_cannot_be_addressed() {
    let engine = MockEngine::start().await;

    let mut item = mock_item(MOCK_ITEM_NAME);
    item["metadata"]["family"] = serde_json::Value::Null;

    engine
        .get_ok("/items", mock_list_envelope(vec![item], None))
        .await;

    let response = engine
        .client(mock_identity())
        .list_items(&ListQuery::default())
        .await
        .expect("the listing itself succeeds");

    let item = &response.value.items[0];
    let error = ItemAddress::from_manifest(
        &item.api_version,
        item.metadata.family.as_deref(),
        &item.metadata.name,
    )
    .expect_err("a null family is unaddressable");

    assert_eq!(error.code, codes::UNADDRESSABLE_ITEM);
}

/// The mock engine must be able to emit a paginated list whose `metadata.continue` walks exactly
/// once — §12.5's own requirement — and `paginate_all` must follow it.
#[rstest]
#[tokio::test]
async fn test_a_paginated_listing_walks_exactly_once() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .and(wiremock::matchers::query_param_is_missing("continue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item("example-item-1")],
            Some("engine-token-2"),
        )))
        .expect(1)
        .mount(engine.server())
        .await;

    Mock::given(method("GET"))
        .and(path("/items"))
        .and(wiremock::matchers::query_param(
            "continue",
            "engine-token-2",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![mock_item("example-item-2")], None)),
        )
        .expect(1)
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());

    let all = paginate_all(|cursor| {
        let client = client.clone();
        async move {
            let response = client
                .list_items(&ListQuery {
                    cursor,
                    ..ListQuery::default()
                })
                .await?;

            Ok::<ListPage<_>, crate::error::ToolError>(response.value)
        }
    })
    .await
    .expect("two pages walk cleanly");

    let names: Vec<&str> = all.iter().map(|item| item.metadata.name.as_str()).collect();

    assert_eq!(names, vec!["example-item-1", "example-item-2"]);
}

// ---------------------------------------------------------------------------------------------
// §8.4 — whose fault a `400` is, decided by what the request carried.
// ---------------------------------------------------------------------------------------------

/// `field`, `label` and `sort` are the caller's; `limit`, the cursor and `rawq` are ours.
#[rstest]
#[case::nothing_of_the_callers(ListQuery::default(), crate::error::BadRequestOrigin::ServerBuilt)]
#[case::only_our_paging(
    ListQuery { limit: Some(200), cursor: Some(EngineCursor::new("abc")), ..ListQuery::default() },
    crate::error::BadRequestOrigin::ServerBuilt
)]
#[case::rawq_is_ours(
    ListQuery { raw_query: vec!["eyJ9".to_string()], ..ListQuery::default() },
    crate::error::BadRequestOrigin::ServerBuilt
)]
#[case::a_field_is_the_callers(
    ListQuery { field: vec!["spec.names.kind=Service".to_string()], ..ListQuery::default() },
    crate::error::BadRequestOrigin::CallerInput
)]
#[case::a_label_is_the_callers(
    ListQuery { label: vec!["tier=gold".to_string()], ..ListQuery::default() },
    crate::error::BadRequestOrigin::CallerInput
)]
#[case::a_sort_is_the_callers(
    ListQuery { sort: vec!["metadata.name".to_string()], ..ListQuery::default() },
    crate::error::BadRequestOrigin::CallerInput
)]
fn test_a_listings_bad_request_origin_follows_what_it_carries(
    #[case] query: ListQuery,
    #[case] expected: crate::error::BadRequestOrigin,
) {
    assert_eq!(query.bad_request_origin(), expected);
}

/// A `400` on a listing built entirely by us is **our** defect, and the model is told so rather
/// than being asked to change arguments it never sent (T1 §9).
#[rstest]
#[tokio::test]
async fn test_a_400_on_a_listing_we_built_is_a_server_defect() {
    let engine = MockEngine::start().await;
    engine
        .get_error(
            "/mia-platform.eu/v1/item-type-definitions",
            400,
            "bad limit",
        )
        .await;

    let error = engine
        .client(mock_identity())
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
            limit: Some(200),
            ..ListQuery::default()
        })
        .await
        .expect_err("a 400 is an error");

    assert_eq!(error.code, codes::SERVER_DEFECT);
    assert_eq!(error.remedy, crate::error::Remedy::Escalate);
}

/// The same `400`, on a listing carrying the caller's filter, stays the caller's to correct.
#[rstest]
#[tokio::test]
async fn test_a_400_on_a_listing_with_the_callers_filter_is_invalid_input() {
    let engine = MockEngine::start().await;
    engine
        .get_error(
            "/mia-platform.eu/v1/item-type-definitions",
            400,
            "bad selector",
        )
        .await;

    let error = engine
        .client(mock_identity())
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
            field: vec!["spec.names.kind=Service".to_string()],
            ..ListQuery::default()
        })
        .await
        .expect_err("a 400 is an error");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, crate::error::Remedy::RetryAfterChange);
}

/// The tenant listing declares no parameters, so nothing in it can be the caller's fault.
#[rstest]
#[tokio::test]
async fn test_a_400_on_the_tenant_listing_is_a_server_defect() {
    let engine = MockEngine::start().await;
    engine.get_error("/bff/tenants", 400, "bad request").await;

    let error = engine
        .client(mock_identity())
        .list_tenants()
        .await
        .expect_err("a 400 is an error");

    assert_eq!(error.code, codes::SERVER_DEFECT);
}

// ---------------------------------------------------------------------------------------------
// T2 — the family listing and the two counts.
// ---------------------------------------------------------------------------------------------

/// The family listing reaches `/{group}/{version}/items/{family}` with the metadata-only
/// projection and nothing but `rawq`, `limit` and the cursor.
#[rstest]
#[tokio::test]
async fn test_the_family_listing_is_addressed_and_projected() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path("/stable.example.com/v1/items/services"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![mock_item(MOCK_ITEM_NAME)], None)),
        )
        .mount(engine.server())
        .await;

    let page = engine
        .client(mock_identity())
        .list_family_items_partial(
            &mock_family(),
            &ListQuery {
                limit: Some(50),
                raw_query: vec!["eyJhIjoxfQ".to_string()],
                ..ListQuery::default()
            },
        )
        .await
        .expect("the listing succeeds")
        .value;

    assert_eq!(page.items[0].metadata.name, MOCK_ITEM_NAME);

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");
    assert_eq!(
        requests[0]
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok()),
        Some(crate::projection::Projection::PartialObjectMetadata.accept())
    );
    assert_eq!(
        requests[0].url.query(),
        Some("limit=50&rawq=eyJhIjoxfQ"),
        "only the paging and the query are sent"
    );
}

/// Both counts read `{count}` and send the listing's `rawq` — and **never** `limit` or the
/// cursor, which a count does not take.
#[rstest]
#[tokio::test]
async fn test_the_counts_send_only_the_query() {
    let engine = MockEngine::start().await;
    for count_path in [
        "/items/count",
        "/stable.example.com/v1/items/services/count",
    ] {
        Mock::given(method("GET"))
            .and(path(count_path))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "count": 128 })),
            )
            .mount(engine.server())
            .await;
    }
    let client = engine.client(mock_identity());
    let query = ListQuery {
        limit: Some(50),
        cursor: Some(EngineCursor::new("next-page")),
        raw_query: vec!["eyJhIjoxfQ".to_string()],
        ..ListQuery::default()
    };

    let global = client
        .count_items(&query)
        .await
        .expect("the count succeeds");
    let family = client
        .count_family_items(&mock_family(), &query)
        .await
        .expect("the count succeeds");

    assert_eq!((global.value.count, family.value.count), (128, 128));
    for request in engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
    {
        assert_eq!(
            request.url.query(),
            Some("rawq=eyJhIjoxfQ"),
            "{}",
            request.url.path()
        );
    }
}
