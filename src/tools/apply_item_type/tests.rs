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
    tools::apply_item_type::{
        ApplyItemType, ApplyItemTypeInput, MAX_KIND_BYTES,
        report::{
            CREATE_ONLY_REASON, IDENTITY_REASON, READ_ONLY_REASON, RETENTION_TRIM_JOB,
            REVISION_BACKFILL_JOB,
        },
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item_type_definition,
        mock_list_envelope,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    http::Method,
    matchers::{method, path, query_param},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The fixture definition's own path.
const TYPE_PATH: &str = "/mia-platform.eu/v1/item-type-definitions/services.stable.example.com";

/// The global count `existingItems` comes from.
const COUNT_PATH: &str = "/items/count";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// A read-only warning, exactly as the engine words it (`compare_readonly_fields_and_add_warnings`).
fn read_only_warning(field: &str) -> String {
    format!("'{field}' field is read-only and was ignored during the update.")
}

/// The stored `Service` type: an `llmDescription` to preserve, two served versions.
fn mock_stored_type() -> Value {
    let mut itd = mock_item_type_definition("Service", "services", GROUP);
    itd["metadata"]["description"] = json!("Services.");
    itd["spec"]["llmDescription"] = json!("Use a Service for anything that runs.");
    itd["spec"]["versions"] = json!([
        {
            "name": "v1", "served": true, "deprecated": false,
            "schema": { "openAPIV31Schema": { "type": "object", "properties": {} } }
        },
        {
            "name": "v2", "served": true, "deprecated": false,
            "schema": { "openAPIV31Schema": { "type": "object", "properties": {} } }
        }
    ]);
    itd
}

/// A call context against `base_url`, with no retries so each mock answers once.
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

/// Answers the `kind` lookup for `Service` with `types`.
async fn mount_lookup(engine: &MockEngine, types: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Service"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(types, None)))
        .mount(engine.server())
        .await;
}

/// Answers the definition's pre-read.
async fn mount_read(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(TYPE_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Answers the definition's write.
async fn mount_write(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("PUT"))
        .and(path(TYPE_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Answers the item count.
async fn mount_count(engine: &MockEngine, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(COUNT_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// An engine success carrying one `Warning` per message.
fn mock_written(body: Value, warnings: &[String]) -> ResponseTemplate {
    warnings.iter().fold(
        ResponseTemplate::new(200).set_body_json(body),
        |template, warning| {
            template.append_header("Warning", format!(r#"299 - "{warning}""#).as_str())
        },
    )
}

/// A mock engine where `Service` exists as stored, the write answers `written`.
async fn mock_update(written: ResponseTemplate) -> MockEngine {
    let engine = MockEngine::start().await;
    mount_lookup(&engine, vec![mock_stored_type()]).await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_stored_type()),
    )
    .await;
    mount_write(&engine, written).await;

    engine
}

/// A mock engine where no `Service` type exists, and the write creates `written`.
async fn mock_create(written: Value) -> MockEngine {
    let engine = MockEngine::start().await;
    mount_lookup(&engine, vec![]).await;
    mount_read(&engine, mock_failure(404, "not found")).await;
    mount_write(&engine, ResponseTemplate::new(201).set_body_json(written)).await;

    engine
}

/// Runs the tool and renders its answer exactly as the runtime does — engine warnings included.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    let context = mock_context_at(&engine.server().uri(), CALL_BUDGET);
    let input: ApplyItemTypeInput =
        serde_json::from_value(arguments).expect("the fixture arguments deserialise");

    let output = ApplyItemType.call(&context, input).await?;

    Ok(output.render(context.engine().call_warnings().collected().as_deref()))
}

/// Every `PUT` body the mock received.
async fn writes(engine: &MockEngine) -> Vec<Value> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .into_iter()
        .filter(|request| request.method == Method::PUT)
        .map(|request| serde_json::from_slice(&request.body).unwrap_or(Value::Null))
        .collect()
}

/// A complete definition to create, for `kind` in the fixture group.
fn mock_new_spec() -> Value {
    json!({
        "group": GROUP,
        "names": { "plural": "services", "singular": "service" },
        "scope": "Tenant",
        "versions": [{
            "name": "v1", "served": true,
            "schema": { "openAPIV31Schema": { "type": "object" } }
        }]
    })
}

// ---------------------------------------------------------------------------------------------
// The `ignored` path, first. A `200` that dropped part of what was sent must say so.
// ---------------------------------------------------------------------------------------------

/// Each read-only field the engine discarded is named in `ignored`, with its reason, and the
/// engine's own words stay in `warnings`.
#[rstest]
#[tokio::test]
async fn test_each_ignored_field_is_named() {
    let warnings = [
        read_only_warning("spec.scope"),
        read_only_warning("spec.names.kind"),
    ];
    let engine = mock_update(mock_written(mock_stored_type(), &warnings)).await;

    let answer = run(
        &engine,
        json!({
            "kind": "Service",
            "spec": { "scope": "Cluster", "names": { "kind": "Svc" } }
        }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(
        answer["ignored"],
        json!([
            { "field": "spec.scope", "reason": READ_ONLY_REASON },
            { "field": "spec.names.kind", "reason": READ_ONLY_REASON }
        ])
    );
    assert_eq!(answer["warnings"], json!(warnings));
    assert_eq!(answer["changed"], json!([]));
}

/// With nothing ignored, `ignored` is present and empty — never omitted.
#[rstest]
#[tokio::test]
async fn test_nothing_ignored_is_an_empty_list() {
    let mut written = mock_stored_type();
    written["spec"]["names"]["displaySingular"] = json!("Service");
    let engine = mock_update(mock_written(written, &[])).await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "spec": { "names": { "displaySingular": "Service" } } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(answer["ignored"], json!([]));
    assert_eq!(answer["backgroundJobs"], json!([]));
    assert_eq!(answer["changed"], json!(["spec.names.displaySingular"]));
}

/// Enabling history on an existing type is reported as out of reach, not as done.
#[rstest]
#[tokio::test]
async fn test_history_on_update_is_refused_informatively() {
    let engine = mock_update(mock_written(
        mock_stored_type(),
        &[read_only_warning("spec.history")],
    ))
    .await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "spec": { "history": { "enabled": false } } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(
        answer["ignored"],
        json!([{ "field": "spec.history", "reason": CREATE_ONLY_REASON }])
    );
    assert!(
        !answer["changed"]
            .as_array()
            .is_some_and(|changed| changed.contains(&json!("spec.history"))),
        "`changed` must not claim what was ignored"
    );
    assert_eq!(answer["backgroundJobs"], json!([]));
}

/// The fields that address the type are held back and reported, not sent: the engine
/// would refuse the whole write over the name.
#[rstest]
#[tokio::test]
async fn test_the_types_address_is_held_back() {
    let engine = mock_update(mock_written(mock_stored_type(), &[])).await;

    let answer = run(
        &engine,
        json!({
            "kind": "Service",
            "spec": { "group": "other.example.com", "names": { "plural": "svcs" } },
            "metadata": { "name": "svcs.other.example.com" }
        }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(
        answer["ignored"],
        json!([
            { "field": "spec.group", "reason": IDENTITY_REASON },
            { "field": "spec.names.plural", "reason": IDENTITY_REASON },
            { "field": "metadata.name", "reason": IDENTITY_REASON }
        ])
    );

    let sent = &writes(&engine).await[0];
    assert_eq!(sent["spec"]["group"], json!(GROUP));
    assert_eq!(sent["spec"]["names"]["plural"], json!("services"));
    assert_eq!(
        sent["metadata"]["name"],
        json!("services.stable.example.com")
    );
}

// ---------------------------------------------------------------------------------------------
// The merge: what the caller did not mention survives — `llmDescription` above all.
// ---------------------------------------------------------------------------------------------

/// A `metadata`-only update leaves `spec.llmDescription`, and every version, where they were.
#[rstest]
#[tokio::test]
async fn test_llm_description_survives_a_metadata_only_update() {
    let mut written = mock_stored_type();
    written["metadata"]["description"] = json!("Anything that runs.");
    let engine = mock_update(mock_written(written, &[])).await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "metadata": { "description": "Anything that runs." } }),
    )
    .await
    .expect("the write succeeds");

    let sent = &writes(&engine).await[0];
    assert_eq!(
        sent["spec"]["llmDescription"],
        mock_stored_type()["spec"]["llmDescription"]
    );
    assert_eq!(
        sent["spec"]["versions"],
        mock_stored_type()["spec"]["versions"]
    );
    assert_eq!(sent["resourceVersion"], json!("1"));
    assert_eq!(answer["changed"], json!(["metadata.description"]));
    assert_eq!(answer["schemaChanged"], json!(false));
    assert!(answer.get("existingItems").is_none());
}

// ---------------------------------------------------------------------------------------------
// The schema moved, a served version went away.
// ---------------------------------------------------------------------------------------------

/// A changed schema is reported with the count of the items stored under the type, across every
/// version — one global count on `kind` and each `apiVersion`.
#[rstest]
#[tokio::test]
async fn test_a_schema_change_is_reported_with_the_item_count() {
    let mut written = mock_stored_type();
    written["spec"]["versions"][0]["schema"]["openAPIV31Schema"]["required"] = json!(["spec"]);
    let engine = mock_update(mock_written(written.clone(), &[])).await;
    mount_count(
        &engine,
        ResponseTemplate::new(200).set_body_json(json!({ "count": 340 })),
    )
    .await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "spec": { "versions": written["spec"]["versions"] } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(answer["schemaChanged"], json!(true));
    assert_eq!(answer["existingItems"], json!(340));
    assert!(answer.get("versionsRemoved").is_none());

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");
    let count = requests
        .iter()
        .find(|request| request.url.path() == COUNT_PATH)
        .expect("the items were counted");
    let rawq = count
        .url
        .query_pairs()
        .find(|(key, _)| key == "rawq")
        .map(|(_, value)| value.into_owned())
        .expect("a rawq was sent");
    let decoded = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        String::from_utf8(URL_SAFE_NO_PAD.decode(rawq).expect("rawq is base64url"))
            .expect("rawq is UTF-8")
    };
    assert!(
        decoded.contains("stable.example.com/v1") && decoded.contains("stable.example.com/v2"),
        "{decoded}"
    );
    assert!(decoded.contains(r#""kind""#), "{decoded}");
}

/// A failed count keeps the flag and says why the number is missing.
#[rstest]
#[tokio::test]
async fn test_a_failed_count_keeps_the_flag() {
    let mut written = mock_stored_type();
    written["spec"]["versions"][0]["schema"]["openAPIV31Schema"]["required"] = json!(["spec"]);
    let engine = mock_update(mock_written(written.clone(), &[])).await;
    mount_count(&engine, mock_failure(503, "unavailable")).await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "spec": { "versions": written["spec"]["versions"] } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(answer["schemaChanged"], json!(true));
    assert!(answer.get("existingItems").is_none());
    assert!(
        answer["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("could not be counted")),
        "{answer}"
    );
}

/// A served version that is no longer served is named, with the items counted.
#[rstest]
#[tokio::test]
async fn test_a_removed_served_version_is_reported() {
    let mut written = mock_stored_type();
    written["spec"]["versions"] = json!([written["spec"]["versions"][1].clone()]);
    let engine = mock_update(mock_written(written.clone(), &[])).await;
    mount_count(
        &engine,
        ResponseTemplate::new(200).set_body_json(json!({ "count": 12 })),
    )
    .await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "spec": { "versions": written["spec"]["versions"] } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(answer["versionsRemoved"], json!(["v1"]));
    assert_eq!(answer["existingItems"], json!(12));
}

// ---------------------------------------------------------------------------------------------
// The create path, and its background jobs.
// ---------------------------------------------------------------------------------------------

/// A kind no type has, with a complete definition, is created — its address derived, its
/// identity filled in, and the existence of the name checked first.
#[rstest]
#[tokio::test]
async fn test_a_new_kind_is_created() {
    let engine = mock_create(mock_item_type_definition("Service", "services", GROUP)).await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "spec": mock_new_spec(), "metadata": { "description": "New." } }),
    )
    .await
    .expect("the create succeeds");

    assert_eq!(answer["created"], json!(true));
    assert_eq!(answer["name"], json!("services.stable.example.com"));

    let sent = &writes(&engine).await[0];
    assert_eq!(sent["apiVersion"], json!("mia-platform.eu/v1"));
    assert_eq!(sent["kind"], json!("ItemTypeDefinition"));
    assert_eq!(
        sent["metadata"]["name"],
        json!("services.stable.example.com")
    );
    assert_eq!(sent["metadata"]["description"], json!("New."));
    assert_eq!(sent["spec"]["names"]["kind"], json!("Service"));
    assert!(sent.get("resourceVersion").is_none());
}

/// Creating with history enabled names the jobs it starts — the trim among them, since it deletes.
#[rstest]
#[case::default_retention(json!({ "enabled": true }), vec![REVISION_BACKFILL_JOB, RETENTION_TRIM_JOB])]
#[case::retain_all(
    json!({ "enabled": true, "retention": { "policy": "All" } }),
    vec![REVISION_BACKFILL_JOB]
)]
#[case::disabled(json!({ "enabled": false }), vec![])]
#[tokio::test]
async fn test_a_created_types_background_jobs_are_named(
    #[case] history: Value,
    #[case] jobs: Vec<&str>,
) {
    let mut created = mock_item_type_definition("Service", "services", GROUP);
    created["spec"]["history"] = history.clone();
    let engine = mock_create(created).await;
    let mut spec = mock_new_spec();
    spec["history"] = history;

    let answer = run(&engine, json!({ "kind": "Service", "spec": spec }))
        .await
        .expect("the create succeeds");

    assert_eq!(answer["backgroundJobs"], json!(jobs));
}

/// An update of a history-enabled type starts nothing new, and says so.
#[rstest]
#[tokio::test]
async fn test_an_update_starts_no_background_jobs() {
    let mut stored = mock_stored_type();
    stored["spec"]["history"] = json!({ "enabled": true });
    let engine = MockEngine::start().await;
    mount_lookup(&engine, vec![stored.clone()]).await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(stored.clone()),
    )
    .await;
    mount_write(&engine, mock_written(stored, &[])).await;

    let answer = run(
        &engine,
        json!({ "kind": "Service", "metadata": { "description": "Services." } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(answer["backgroundJobs"], json!([]));
}

/// A create missing what makes a type addressable is refused before anything is written.
#[rstest]
#[case::no_versions(json!({ "group": GROUP, "names": { "plural": "services" } }), "spec.versions")]
#[case::empty_versions(
    json!({ "group": GROUP, "names": { "plural": "services" }, "versions": [] }),
    "spec.versions"
)]
#[case::no_plural(json!({ "group": GROUP, "versions": [{ "name": "v1" }] }), "spec.names.plural")]
#[case::no_group(json!({ "names": { "plural": "services" }, "versions": [{ "name": "v1" }] }), "spec.group")]
#[case::other_kind(
    json!({
        "group": GROUP, "names": { "plural": "services", "kind": "Svc" },
        "versions": [{ "name": "v1" }]
    }),
    "spec.names.kind"
)]
#[tokio::test]
async fn test_an_incomplete_create_writes_nothing(#[case] spec: Value, #[case] field: &str) {
    let engine = mock_create(json!({})).await;

    let error = run(&engine, json!({ "kind": "Service", "spec": spec }))
        .await
        .expect_err("the create is incomplete");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error.details.as_deref().map(|details| &details["field"]),
        Some(&json!(field))
    );
    assert!(writes(&engine).await.is_empty());
}

/// `group` and `spec.group` must agree on a create.
#[rstest]
#[tokio::test]
async fn test_a_create_with_two_groups_writes_nothing() {
    let engine = mock_create(json!({})).await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "kind": "Service", "group": "other.example.com", "spec": mock_new_spec() }),
    )
    .await
    .expect_err("the groups disagree");

    assert_eq!(
        error.details.as_deref().map(|details| &details["field"]),
        Some(&json!("group"))
    );
    assert!(writes(&engine).await.is_empty());
}

/// An unknown kind with nothing that says "create" is most likely a misspelling: near matches,
/// and nothing written.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_without_a_definition_offers_near_matches() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.names.kind=Servic"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![mock_stored_type()], None)),
        )
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "kind": "Servic", "metadata": { "description": "Services." } }),
    )
    .await
    .expect_err("an unknown kind is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!(["Service"]))
    );
    assert!(
        error
            .next_step
            .as_deref()
            .is_some_and(|step| step.contains("to create one"))
    );
    assert!(writes(&engine).await.is_empty());
}

/// A create whose name another kind already owns writes nothing (the cycle's `Absent` check).
#[rstest]
#[tokio::test]
async fn test_a_create_over_another_kinds_name_writes_nothing() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;
    mount_read(
        &engine,
        ResponseTemplate::new(200).set_body_json(mock_stored_type()),
    )
    .await;
    mount_write(&engine, ResponseTemplate::new(200)).await;

    let error = run(&engine, json!({ "kind": "Svc", "spec": mock_new_spec() }))
        .await
        .expect_err("the name is taken");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert!(writes(&engine).await.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The remaining error rows.
// ---------------------------------------------------------------------------------------------

/// A `409` is reported once, never retried.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_reported_not_retried() {
    let engine = mock_update(mock_failure(409, "Concurrent modification")).await;

    let error = run(
        &engine,
        json!({ "kind": "Service", "metadata": { "description": "Changed." } }),
    )
    .await
    .expect_err("a conflict is an error");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, Remedy::RetryLater);
    assert_eq!(writes(&engine).await.len(), 1);
}

/// An invalid schema names the version and the place inside its schema.
#[rstest]
#[tokio::test]
async fn test_an_invalid_schema_names_its_location() {
    let message = "Body field 'spec.versions' is invalid: 'spec.versions[1].schema.openAPIV31Schema' \
                   is not valid: path \"/properties/spec/type\": \"strin\" is not valid under any of \
                   the schemas listed in the 'anyOf' keyword";
    let engine = mock_update(mock_failure(400, message)).await;

    let error = run(
        &engine,
        json!({ "kind": "Service", "spec": { "versions": [] } }),
    )
    .await
    .expect_err("the definition is invalid");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    let details = error.details.expect("details");
    assert_eq!(
        details["path"],
        json!("spec.versions[1].schema.openAPIV31Schema")
    );
    assert_eq!(details["schemaPath"], json!("/properties/spec/type"));
}

/// A failure after the `PUT` left may have landed; one on the pre-read wrote nothing.
#[rstest]
#[tokio::test]
async fn test_the_two_5xx_sides_stay_apart() {
    let engine = mock_update(mock_failure(500, "Something went wrong")).await;
    let error = run(
        &engine,
        json!({ "kind": "Service", "metadata": { "description": "Changed." } }),
    )
    .await
    .expect_err("a failed write is an error");
    assert_eq!(
        (error.code, error.remedy),
        (codes::UNKNOWN_OUTCOME, Remedy::Unknown)
    );

    let engine = MockEngine::start().await;
    mount_lookup(&engine, vec![mock_stored_type()]).await;
    mount_read(&engine, mock_failure(503, "unavailable")).await;
    let error = run(
        &engine,
        json!({ "kind": "Service", "metadata": { "description": "Changed." } }),
    )
    .await
    .expect_err("a failed read is an error");
    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
    assert!(writes(&engine).await.is_empty());
}

/// A core kind's items are not in the global count: the number is withheld, not reported as `0`.
#[rstest]
#[tokio::test]
async fn test_a_core_kinds_items_are_not_miscounted() {
    let mut stored = mock_item_type_definition("Relationship", "relationships", "mia-platform.eu");
    stored["spec"]["versions"][0]["schema"]["openAPIV31Schema"] = json!({ "type": "object" });
    let mut written = stored.clone();
    written["spec"]["versions"][0]["schema"]["openAPIV31Schema"]["required"] = json!(["spec"]);
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![stored.clone()], None)),
        )
        .mount(engine.server())
        .await;
    let type_path = "/mia-platform.eu/v1/item-type-definitions/relationships.mia-platform.eu";
    Mock::given(method("GET"))
        .and(path(type_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(stored))
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path(type_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(written.clone()))
        .mount(engine.server())
        .await;

    let answer = run(
        &engine,
        json!({ "kind": "Relationship", "spec": { "versions": written["spec"]["versions"] } }),
    )
    .await
    .expect("the write succeeds");

    assert_eq!(answer["schemaChanged"], json!(true));
    assert!(answer.get("existingItems").is_none());
    assert!(
        answer["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("does not count")),
        "{answer}"
    );
}

// ---------------------------------------------------------------------------------------------
// Arguments checked before anything reaches the engine.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::empty_kind(json!({ "kind": "" }), "kind")]
#[case::long_kind(json!({ "kind": "A".repeat(MAX_KIND_BYTES + 1) }), "kind")]
#[case::malformed_kind(json!({ "kind": "my-kind" }), "kind")]
#[case::malformed_group(json!({ "kind": "Service", "group": "Not A Group" }), "group")]
#[case::spec_not_an_object(json!({ "kind": "Service", "spec": [1] }), "spec")]
#[case::spec_null(json!({ "kind": "Service", "spec": null }), "spec")]
#[case::metadata_not_an_object(json!({ "kind": "Service", "metadata": "x" }), "metadata")]
#[tokio::test]
async fn test_a_malformed_argument_is_refused_before_the_engine(
    #[case] arguments: Value,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&engine, arguments)
        .await
        .expect_err("the argument is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(
        error.details.as_deref().map(|details| &details["field"]),
        Some(&json!(field))
    );
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
}
