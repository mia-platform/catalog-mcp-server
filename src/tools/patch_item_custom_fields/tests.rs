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
    tools::patch_item_custom_fields::{
        MAX_FIELDS, MAX_KEY_BYTES, PatchItemCustomFields, PatchItemCustomFieldsInput,
        discovery::{self, KeyClass},
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError, TypeCoordinates,
    error::codes,
    models::CustomFieldSpec,
    testing::{
        MOCK_ITEM_NAME, MockEngine, mock_acl_context, mock_error_body, mock_item,
        mock_item_type_definition, mock_list_envelope,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    http::Method,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// Where the fixture item lives: `mock_item`'s group, version and family.
const ITEM_PATH: &str = "/stable.example.com/v1/items/services/example-item";

/// Where its custom fields are patched.
const PATCH_PATH: &str = "/stable.example.com/v1/items/services/example-item/custom-fields";

/// Where the custom-field definitions are listed.
const FIELDS_PATH: &str = "/mia-platform.eu/v1/items/custom-fields";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The fixture type, as `applicableTo` names it.
const TYPE_URN: &str = "urn:mia-platform-catalog:stable.example.com:v1:Service";

/// Another type, as `applicableTo` names it.
const OTHER_TYPE_URN: &str = "urn:mia-platform-catalog:example.com:v1:Database";

/// The engine's refusal of an unknown or inapplicable key, verbatim in shape.
const UNKNOWN_KEY_MESSAGE: &str = "Unknown custom field key 'tier': it is not registered or not \
     applicable to this item type";

/// The engine's refusal of a value, verbatim in shape.
const INVALID_VALUE_MESSAGE: &str = "Value for custom field 'sensitivity' is invalid: \"top\" is \
     not one of [\"public\",\"internal\"]";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// A call context against `engine`, with `retries` transport retries and a `timeout` per request.
fn mock_context_with(engine: &MockEngine, retries: u8, timeout: Duration) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        Some("Bearer test-token"),
        None,
    ));
    let client = EngineClientFactory::new(
        &engine.server().uri(),
        "/",
        timeout,
        Duration::from_millis(200),
        retries,
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

/// An input, deserialised exactly as the runtime deserialises a call's arguments — which is what
/// keeps an explicit `null` a removal.
fn mock_input(arguments: Value) -> PatchItemCustomFieldsInput {
    serde_json::from_value(arguments).expect("the fixture arguments deserialise")
}

/// The arguments of a call on the fixture item carrying `fields`.
fn mock_arguments(fields: Value) -> Value {
    json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "fields": fields })
}

/// A `CustomField` item named `name`, with `key`, a string schema, and `applicable_to` when given.
fn mock_field(name: &str, key: &str, applicable_to: Option<Value>) -> Value {
    let mut spec = json!({ "key": key, "schema": { "type": "string" } });
    if let Some(types) = applicable_to {
        spec["applicableTo"] = types;
    }

    json!({
        "apiVersion": "mia-platform.eu/v1",
        "kind": "CustomField",
        "metadata": { "name": name },
        "spec": spec,
    })
}

/// The fixture item, carrying `custom_fields`.
fn mock_item_with(custom_fields: Option<Value>) -> Value {
    let mut item = mock_item(MOCK_ITEM_NAME);
    if let Some(custom_fields) = custom_fields {
        item["customFields"] = custom_fields;
    }

    item
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// A mock engine that knows the `Service` type.
async fn mock_engine() -> MockEngine {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_type_definition("Service", "services", GROUP)],
            None,
        )))
        .mount(engine.server())
        .await;

    engine
}

/// Answers the patch.
async fn mount_patch(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("PATCH"))
        .and(path(PATCH_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Answers the definitions listing with one page.
async fn mount_fields(engine: &MockEngine, fields: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(FIELDS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(fields, None)))
        .mount(engine.server())
        .await;
}

/// The definitions most tests classify against: `sensitivity` for this type, `test-type` for
/// another, `legacy` for none, and `scope` for every type.
fn mock_definitions() -> Vec<Value> {
    vec![
        mock_field("sensitivity", "sensitivity", Some(json!([TYPE_URN]))),
        mock_field("test-type", "test-type", Some(json!([OTHER_TYPE_URN]))),
        mock_field("legacy", "legacy", Some(json!([]))),
        mock_field("scope", "scope", None),
    ]
}

/// Runs the tool against `engine`, with no retries.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    run_with(engine, arguments)
        .await
        .map(|output| output.payload().clone())
}

/// Runs the tool, returning the whole output — warnings included.
async fn run_with(
    engine: &MockEngine,
    arguments: Value,
) -> Result<crate::registry::contract::ToolOutput, ToolError> {
    let context = mock_context_with(engine, 0, Duration::from_secs(5));

    PatchItemCustomFields
        .call(&context, mock_input(arguments))
        .await
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

/// The error's details, as JSON.
fn details(error: &ToolError) -> Value {
    error.details.as_deref().cloned().unwrap_or(Value::Null)
}

/// The fixture type's coordinates, at `version`.
fn mock_coordinates(version: &str) -> TypeCoordinates {
    TypeCoordinates {
        group: GROUP.to_string(),
        version: version.to_string(),
        family: "services".to_string(),
        kind: "Service".to_string(),
        type_name: format!("services.{GROUP}"),
        selectable_fields: Vec::new(),
        history_enabled: true,
    }
}

// ---------------------------------------------------------------------------------------------
// The content type, first: a `415` would only show in integration.
// ---------------------------------------------------------------------------------------------

/// The patch carries **one** `Content-Type`, the merge-patch one.
#[rstest]
#[tokio::test]
async fn test_the_merge_patch_content_type_is_sent_exactly_once() {
    let engine = mock_engine().await;
    mount_patch(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_item_with(None)),
    )
    .await;

    run(&engine, mock_arguments(json!({ "sensitivity": "high" })))
        .await
        .expect("the patch lands");

    let patches = requests_on(&engine, Method::PATCH, PATCH_PATH).await;
    assert_eq!(patches.len(), 1);
    let content_types: Vec<_> = patches[0]
        .headers
        .get_all(http::header::CONTENT_TYPE)
        .iter()
        .collect();
    assert_eq!(content_types, vec!["application/merge-patch+json"]);
}

// ---------------------------------------------------------------------------------------------
// The write.
// ---------------------------------------------------------------------------------------------

/// One `PATCH`, with no query string — no `resourceVersion` — and no read of the item: the engine
/// merges and guards the write itself. The body is the caller's keys, `null` kept.
#[rstest]
#[tokio::test]
async fn test_a_write_is_one_patch_without_a_read_or_a_resource_version() {
    let engine = mock_engine().await;
    mount_patch(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_item_with(None)),
    )
    .await;

    run(
        &engine,
        mock_arguments(json!({ "sensitivity": "high", "owner-team": null })),
    )
    .await
    .expect("the patch lands");

    let patches = requests_on(&engine, Method::PATCH, PATCH_PATH).await;
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].url.query(), None);
    let body: Value = serde_json::from_slice(&patches[0].body).expect("a JSON body");
    assert_eq!(body, json!({ "owner-team": null, "sensitivity": "high" }));
    assert!(
        requests_on(&engine, Method::GET, ITEM_PATH)
            .await
            .is_empty()
    );
}

/// `set` and `removed` are the request's, sorted; `customFields` is what the engine answered — a
/// value replaced whole, as the engine stored it, never a merge of ours.
#[rstest]
#[tokio::test]
async fn test_the_answer_carries_the_engines_map_and_the_keys_as_requested() {
    let engine = mock_engine().await;
    let stored = json!({ "sensitivity": "high", "limits": { "cpu": 2 } });
    mount_patch(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_item_with(Some(stored.clone()))),
    )
    .await;

    let answer = run(
        &engine,
        mock_arguments(json!({
            "sensitivity": "high", "limits": { "cpu": 2 }, "owner-team": null, "zone": null
        })),
    )
    .await
    .expect("the patch lands");

    assert_eq!(
        answer,
        json!({
            "name": MOCK_ITEM_NAME, "kind": "Service", "group": GROUP, "version": "v1",
            "family": "services",
            "set": ["limits", "sensitivity"],
            "removed": ["owner-team", "zone"],
            "customFields": stored,
        })
    );
}

/// An item left with no custom fields answers `{}`, not an absent map.
#[rstest]
#[tokio::test]
async fn test_an_item_without_custom_fields_answers_an_empty_map() {
    let engine = mock_engine().await;
    mount_patch(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_item_with(None)),
    )
    .await;

    let answer = run(&engine, mock_arguments(json!({ "owner-team": null })))
        .await
        .expect("the patch lands");

    assert_eq!(answer["customFields"], json!({}));
    assert_eq!(answer["set"], json!([]));
    assert_eq!(answer["removed"], json!(["owner-team"]));
}

/// A `409` arrives only after the engine's own attempts; it is reported, never retried here.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_reported_not_retried() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(409, "Concurrent modification")).await;

    let error = run(&engine, mock_arguments(json!({ "sensitivity": "high" })))
        .await
        .expect_err("the engine gave up");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CONFLICT, Remedy::RetryLater)
    );
    assert_eq!(
        requests_on(&engine, Method::PATCH, PATCH_PATH).await.len(),
        1
    );
}

/// A gateway failure is retried, as `apply_item`'s write is: the patch is idempotent.
#[rstest]
#[tokio::test]
async fn test_a_gateway_failure_is_retried() {
    let engine = mock_engine().await;
    Mock::given(method("PATCH"))
        .and(path(PATCH_PATH))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(engine.server())
        .await;
    mount_patch(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_item_with(None)),
    )
    .await;

    let context = mock_context_with(&engine, 1, Duration::from_secs(5));
    PatchItemCustomFields
        .call(
            &context,
            mock_input(mock_arguments(json!({ "sensitivity": "high" }))),
        )
        .await
        .expect("the retry lands");

    assert_eq!(
        requests_on(&engine, Method::PATCH, PATCH_PATH).await.len(),
        2
    );
}

// ---------------------------------------------------------------------------------------------
// Discovery: an empty `fields` reads, and never writes.
// ---------------------------------------------------------------------------------------------

/// Empty `fields` reads the item and the definitions, sends **no** `PATCH`, and answers the map
/// and the keys this type can take — `scope`, for every type, included; `legacy` and `test-type`
/// not.
#[rstest]
#[tokio::test]
async fn test_empty_fields_discovers_without_writing() {
    let engine = mock_engine().await;
    engine
        .get_ok(
            ITEM_PATH,
            mock_item_with(Some(json!({ "sensitivity": "high" }))),
        )
        .await;
    mount_fields(&engine, mock_definitions()).await;

    let output = run_with(&engine, mock_arguments(json!({})))
        .await
        .expect("discovery answers");

    assert_eq!(
        output.payload(),
        &json!({
            "name": MOCK_ITEM_NAME, "kind": "Service", "group": GROUP, "version": "v1",
            "family": "services",
            "customFields": { "sensitivity": "high" },
            "validKeys": { "scope": { "type": "string" }, "sensitivity": { "type": "string" } },
        })
    );
    assert!(
        requests_on(&engine, Method::PATCH, PATCH_PATH)
            .await
            .is_empty()
    );
    assert_eq!(
        output.render(None)["warnings"],
        json!([super::DISCOVERY_WARNING])
    );
}

/// When the definitions cannot be read, discovery still answers the map, and says why there are
/// no keys.
#[rstest]
#[tokio::test]
async fn test_discovery_without_the_definitions_answers_the_map() {
    let engine = mock_engine().await;
    engine.get_ok(ITEM_PATH, mock_item_with(None)).await;
    Mock::given(method("GET"))
        .and(path(FIELDS_PATH))
        .respond_with(mock_failure(403, "Forbidden"))
        .mount(engine.server())
        .await;

    let output = run_with(&engine, mock_arguments(json!({})))
        .await
        .expect("discovery answers");

    assert_eq!(output.payload()["customFields"], json!({}));
    assert!(output.payload().get("validKeys").is_none());
    assert_eq!(
        output.render(None)["warnings"],
        json!([super::DISCOVERY_WARNING, super::DEFINITIONS_UNREADABLE])
    );
}

// ---------------------------------------------------------------------------------------------
// A refused patch, explained.
// ---------------------------------------------------------------------------------------------

/// A refused patch with keys of every class: each named, the valid keys listed, the engine's
/// message kept, and — since `tier` can be corrected — `RetryAfterChange`.
#[rstest]
#[tokio::test]
async fn test_a_refused_patch_classifies_every_key() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, UNKNOWN_KEY_MESSAGE)).await;
    mount_fields(&engine, mock_definitions()).await;

    let error = run(
        &engine,
        mock_arguments(json!({
            "sensitivity": "high", "tier": "gold", "test-type": "unit", "legacy": "x",
            "gone": null
        })),
    )
    .await
    .expect_err("the engine refused the patch");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert!(error.message.starts_with("Nothing was written."));
    assert!(error.message.contains("`tier` is not a custom field"));
    assert!(
        error
            .message
            .contains("`test-type` is a custom field for other item types (Database)")
    );
    assert!(
        error
            .message
            .contains("`legacy` is defined but applies to no item type")
    );
    assert_eq!(
        details(&error),
        json!({
            "keys": {
                "legacy": "noType",
                "sensitivity": "valid",
                "test-type": { "otherTypes": [OTHER_TYPE_URN] },
                "tier": "unknown",
            },
            "validKeys": { "scope": { "type": "string" }, "sensitivity": { "type": "string" } },
            "engineMessage": UNKNOWN_KEY_MESSAGE,
        })
    );
}

/// A valid key beside one meant for no type is still something to write: the model is told to
/// drop the bad key, not to stop.
#[rstest]
#[tokio::test]
async fn test_a_valid_key_beside_an_inapplicable_one_can_be_retried() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, UNKNOWN_KEY_MESSAGE)).await;
    mount_fields(&engine, mock_definitions()).await;

    let error = run(
        &engine,
        mock_arguments(json!({ "sensitivity": "high", "legacy": "x" })),
    )
    .await
    .expect_err("the engine refused the patch");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains("Correct or drop those keys"));
}

/// When every key is meant for other types or for none, nothing in the patch can be written: a
/// person must change the definitions.
#[rstest]
#[tokio::test]
async fn test_only_inapplicable_keys_are_unsupported_for_this_type() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, UNKNOWN_KEY_MESSAGE)).await;
    mount_fields(&engine, mock_definitions()).await;

    let error = run(
        &engine,
        mock_arguments(json!({ "test-type": "unit", "legacy": "x" })),
    )
    .await
    .expect_err("the engine refused the patch");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate)
    );
    assert!(error.message.contains("A person must change"));
}

/// Every key applies: the engine refused a value. Its message is the explanation, with the schema
/// of each key that was given a value.
#[rstest]
#[tokio::test]
async fn test_a_refused_value_relays_the_engine_with_the_schemas_given() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, INVALID_VALUE_MESSAGE)).await;
    mount_fields(&engine, mock_definitions()).await;

    let error = run(
        &engine,
        mock_arguments(json!({ "sensitivity": "top", "gone": null })),
    )
    .await
    .expect_err("the engine refused the value");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error.message,
        format!("Nothing was written. {INVALID_VALUE_MESSAGE}")
    );
    assert_eq!(
        details(&error),
        json!({
            "engineMessage": INVALID_VALUE_MESSAGE,
            "validKeys": { "sensitivity": { "type": "string" } },
        })
    );
}

/// A patch of only `null`s is never checked against the definitions by the engine, so its `400`
/// is about something else: relayed as it is, without listing anything.
#[rstest]
#[tokio::test]
async fn test_a_refused_removal_only_patch_is_relayed() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, "Missing ACL context")).await;

    let error = run(&engine, mock_arguments(json!({ "gone": null })))
        .await
        .expect_err("the engine refused the patch");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.message, "Nothing was written. Missing ACL context");
    assert_eq!(
        details(&error),
        json!({ "engineMessage": "Missing ACL context" })
    );
    assert!(
        requests_on(&engine, Method::GET, FIELDS_PATH)
            .await
            .is_empty()
    );
}

/// The engine's request id survives the explanation: the details are added to, never replaced.
#[rstest]
#[tokio::test]
async fn test_the_request_id_is_kept() {
    let engine = mock_engine().await;
    mount_patch(
        &engine,
        mock_failure(400, UNKNOWN_KEY_MESSAGE).insert_header("x-request-id", "req-42"),
    )
    .await;
    mount_fields(&engine, mock_definitions()).await;

    let error = run(&engine, mock_arguments(json!({ "tier": "gold" })))
        .await
        .expect_err("the engine refused the key");

    assert_eq!(details(&error)["requestId"], json!("req-42"));
    assert_eq!(details(&error)["keys"], json!({ "tier": "unknown" }));
}

/// The definitions are listed from the family endpoint — `/items` excludes it — every page.
#[rstest]
#[tokio::test]
async fn test_the_definitions_are_listed_from_the_family_every_page() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, UNKNOWN_KEY_MESSAGE)).await;
    Mock::given(method("GET"))
        .and(path(FIELDS_PATH))
        .and(query_param_is_missing("continue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_field("scope", "scope", None)],
            Some("page-2"),
        )))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(FIELDS_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_field(
                "tier-field",
                "tier",
                Some(json!([OTHER_TYPE_URN])),
            )],
            None,
        )))
        .mount(engine.server())
        .await;

    let error = run(&engine, mock_arguments(json!({ "tier": "gold" })))
        .await
        .expect_err("the engine refused the key");

    assert_eq!(
        details(&error)["keys"],
        json!({ "tier": { "otherTypes": [OTHER_TYPE_URN] } })
    );
    let listings = requests_on(&engine, Method::GET, FIELDS_PATH).await;
    assert_eq!(listings.len(), 2);
    assert!(listings.iter().all(|request| {
        request
            .url
            .query_pairs()
            .any(|(k, v)| k == "limit" && v == "200")
    }));
    assert!(requests_on(&engine, Method::GET, "/items").await.is_empty());
}

/// When the definitions cannot be read — a caller allowed to write the item but not to read them —
/// the engine's refusal is relayed, saying the keys could not be checked.
#[rstest]
#[tokio::test]
async fn test_a_refused_listing_relays_the_engine() {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(400, UNKNOWN_KEY_MESSAGE)).await;
    Mock::given(method("GET"))
        .and(path(FIELDS_PATH))
        .respond_with(mock_failure(403, "Forbidden"))
        .mount(engine.server())
        .await;

    let error = run(&engine, mock_arguments(json!({ "tier": "gold" })))
        .await
        .expect_err("the engine refused the key");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains(UNKNOWN_KEY_MESSAGE));
    assert!(error.message.ends_with(super::DEFINITIONS_UNREADABLE));
    assert_eq!(details(&error)["engineMessage"], json!(UNKNOWN_KEY_MESSAGE));
}

// ---------------------------------------------------------------------------------------------
// The classification, on its own.
// ---------------------------------------------------------------------------------------------

/// Read a fixture definition.
fn spec(key: &str, applicable_to: Option<Value>) -> CustomFieldSpec {
    let mut field = mock_field("ignored-name", key, applicable_to);
    serde_json::from_value(field["spec"].take()).expect("a well-formed spec")
}

/// A key is matched by `spec.key`, never by the definition's name.
#[rstest]
fn test_a_key_is_matched_by_spec_key_not_by_name() {
    let definitions = vec![spec("cost-centre", Some(json!([TYPE_URN])))];

    assert_eq!(
        discovery::classify(&definitions, "cost-centre", TYPE_URN),
        KeyClass::Valid
    );
    assert_eq!(
        discovery::classify(&definitions, "ignored-name", TYPE_URN),
        KeyClass::Unknown
    );
}

/// A missing `applicableTo` means every type; an empty one means none.
#[rstest]
#[case::missing_is_every_type(None, KeyClass::Valid)]
#[case::empty_is_no_type(Some(json!([])), KeyClass::NoType)]
#[case::listed(Some(json!([TYPE_URN])), KeyClass::Valid)]
#[case::other(Some(json!([OTHER_TYPE_URN])), KeyClass::OtherTypes(vec![OTHER_TYPE_URN.to_string()]))]
fn test_applicable_to_is_read_as_the_engine_reads_it(
    #[case] applicable_to: Option<Value>,
    #[case] expected: KeyClass,
) {
    let definitions = vec![spec("scope", applicable_to)];

    assert_eq!(
        discovery::classify(&definitions, "scope", TYPE_URN),
        expected
    );
}

/// The type URN is the addressed group and version and the resolved kind; a field listed for
/// another version of the same type does not apply, and is said to be that.
#[rstest]
fn test_another_version_of_the_type_is_named_as_such() {
    let coordinates = mock_coordinates("v2");
    let urn = discovery::type_urn(&coordinates);
    assert_eq!(
        urn,
        "urn:mia-platform-catalog:stable.example.com:v2:Service"
    );

    let class = discovery::classify(
        &[spec("sensitivity", Some(json!([TYPE_URN])))],
        "sensitivity",
        &urn,
    );
    assert_eq!(class, KeyClass::OtherTypes(vec![TYPE_URN.to_string()]));

    let reason = discovery::explain("sensitivity", &class, &coordinates).expect("a reason");
    assert!(
        reason.contains("another version of `Service` (v1)"),
        "{reason}"
    );
}

// ---------------------------------------------------------------------------------------------
// The other refusals.
// ---------------------------------------------------------------------------------------------

/// Every other engine refusal keeps the client's mapping, and the two that have an obvious next
/// call carry it.
#[rstest]
#[case::unsupported_media_type(415, codes::SERVER_DEFECT, Remedy::Escalate, None)]
#[case::refusing_kind(422, codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate, None)]
#[case::missing(404, codes::NOT_FOUND, Remedy::RetryAfterChange, Some("describe_item"))]
#[case::forbidden(403, codes::FORBIDDEN, Remedy::Escalate, None)]
#[case::internal(500, codes::UNKNOWN_OUTCOME, Remedy::Unknown, Some("once more"))]
#[tokio::test]
async fn test_the_other_refusals_are_mapped(
    #[case] status: u16,
    #[case] code: &str,
    #[case] remedy: Remedy,
    #[case] next_step: Option<&str>,
) {
    let engine = mock_engine().await;
    mount_patch(&engine, mock_failure(status, "refused")).await;

    let error = run(&engine, mock_arguments(json!({ "sensitivity": "high" })))
        .await
        .expect_err("the engine refused the patch");

    assert_eq!((error.code, error.remedy), (code, remedy));
    match next_step {
        Some(expected) => assert!(
            error
                .next_step
                .as_deref()
                .is_some_and(|step| step.contains(expected)),
            "{:?}",
            error.next_step
        ),
        None => assert!(
            requests_on(&engine, Method::GET, FIELDS_PATH)
                .await
                .is_empty()
        ),
    }
}

/// The engine's `422` reaches the model with the engine's own words, which name the family.
#[rstest]
#[tokio::test]
async fn test_a_refusing_kind_keeps_the_engines_message() {
    let engine = mock_engine().await;
    let message = "Custom fields are not supported for items of family 'views' in API group \
         'mia-platform.eu/v1'";
    mount_patch(&engine, mock_failure(422, message)).await;

    let error = run(&engine, mock_arguments(json!({ "sensitivity": "high" })))
        .await
        .expect_err("the engine refused the kind");

    assert_eq!(error.message, message);
}

/// A write that timed out after it left has an unknown outcome, and says how to find out.
#[rstest]
#[tokio::test]
async fn test_a_timeout_after_the_patch_left_is_an_unknown_outcome() {
    let engine = mock_engine().await;
    mount_patch(
        &engine,
        ResponseTemplate::new(200)
            .set_body_json(mock_item_with(None))
            .set_delay(Duration::from_millis(800)),
    )
    .await;

    let context = mock_context_with(&engine, 0, Duration::from_millis(300));
    let error = PatchItemCustomFields
        .call(
            &context,
            mock_input(mock_arguments(json!({ "sensitivity": "high" }))),
        )
        .await
        .expect_err("the answer came too late");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNKNOWN_OUTCOME, Remedy::Unknown)
    );
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("describe_item"))
    );
}

/// An unknown kind is refused with the lookup's own answer, and nothing is patched.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_patches_nothing() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "name": MOCK_ITEM_NAME, "kind": "Servise", "fields": { "a": "b" } }),
    )
    .await
    .expect_err("no such kind");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert!(
        requests_on(&engine, Method::PATCH, PATCH_PATH)
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------------------------
// The input bounds: refused before anything is sent.
// ---------------------------------------------------------------------------------------------

/// Too many keys, an empty key, an oversized key and a malformed name are refused, naming the
/// parameter.
#[rstest]
#[case::too_many_keys(
    json!({ "name": MOCK_ITEM_NAME, "kind": "Service",
            "fields": (0..=MAX_FIELDS).map(|i| (format!("k{i}"), json!(1))).collect::<serde_json::Map<_, _>>() }),
    "fields"
)]
#[case::empty_key(json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "fields": { "": 1 } }), "fields")]
#[case::oversized_key(
    json!({ "name": MOCK_ITEM_NAME, "kind": "Service", "fields": { "k".repeat(MAX_KEY_BYTES + 1): 1 } }),
    "fields"
)]
#[case::bad_name(json!({ "name": "Not A Name", "kind": "Service", "fields": {} }), "name")]
#[case::bad_kind(json!({ "name": MOCK_ITEM_NAME, "kind": "9lives", "fields": {} }), "kind")]
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

/// A key as long as the bound allows is sent.
#[rstest]
#[tokio::test]
async fn test_a_key_at_the_bound_is_sent() {
    let engine = mock_engine().await;
    mount_patch(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_item_with(None)),
    )
    .await;

    run(
        &engine,
        mock_arguments(json!({ "k".repeat(MAX_KEY_BYTES): null })),
    )
    .await
    .expect("the patch lands");
}

/// `null` reaches the tool as a value, not as an absent key: it is the removal.
#[rstest]
fn test_null_is_kept_as_a_removal() {
    let input = mock_input(mock_arguments(json!({ "gone": null })));

    assert_eq!(input.fields.get("gone"), Some(&Value::Null));
}
