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
    tools::{
        apply_item::ApplyItem,
        patch_item_custom_fields::PatchItemCustomFields,
        restore_item_version::{
            CAMPAIGNS_WARNING, NO_OP_NOTE, OVERTAKEN_WARNING, RestoreItemVersion,
            RestoreItemVersionInput, TRIMMED_WARNING, UNREAD_WARNING,
        },
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item, mock_item_revision,
        mock_item_type_definition, mock_item_version, mock_list_envelope,
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

/// The fixture item.
const ITEM_PATH: &str = "/stable.example.com/v1/items/services/example-item";

/// Its revisions.
const REVISIONS_PATH: &str = "/stable.example.com/v1/items/services/example-item/revisions";

/// Its versions.
const VERSIONS_PATH: &str = "/stable.example.com/v1/items/services/example-item/versions";

/// The restore of version `1.4.0`.
const RESTORE_PATH: &str =
    "/stable.example.com/v1/items/services/example-item/versions/1.4.0/restore";

/// The fixture type's group.
const GROUP: &str = "stable.example.com";

/// The caller, as the gateway would name it.
const CALLER: &str = "019f1cb8-12ec-7f43-94d4-285b77930ce7";

/// The revision the restore records.
const OWN: &str = "01998f1e-0000-7000-8000-000000000001";

/// The revision below it: the state before.
const BELOW: &str = "01998f1e-0000-7000-8000-000000000002";

/// When the restore wrote the item.
const RESTORED_AT: &str = "2026-10-16T10:22:03Z";

/// A time before it.
const EARLIER: &str = "2026-10-16T09:00:00Z";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// A call context against `engine`, as the caller, with `budget` to run in.
fn mock_context(engine: &MockEngine, budget: Duration) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        Some(CALLER),
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
    .bind(identity.clone(), Deadline::starting_now(budget));

    CallContext::new(
        client,
        Deadline::starting_now(budget),
        CancellationToken::new(),
        None,
        identity.tenant_key(),
    )
}

/// Runs the tool with `budget`, returning its whole output.
async fn run_within(
    engine: &MockEngine,
    arguments: Value,
    budget: Duration,
) -> Result<ToolOutput, ToolError> {
    let input: RestoreItemVersionInput =
        serde_json::from_value(arguments).expect("the fixture arguments deserialise");

    RestoreItemVersion
        .call(&mock_context(engine, budget), input)
        .await
}

/// Runs the tool, returning its whole output.
async fn run_with(engine: &MockEngine, arguments: Value) -> Result<ToolOutput, ToolError> {
    run_within(engine, arguments, CALL_BUDGET).await
}

/// Runs the tool, returning its payload.
async fn run(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    run_with(engine, arguments)
        .await
        .map(|output| output.payload().clone())
}

/// A call restoring the fixture item to `1.4.0`.
fn mock_arguments() -> Value {
    json!({ "name": "example-item", "kind": "Service", "versionName": "1.4.0" })
}

/// The tool's own warnings on `output`.
fn warnings(output: &ToolOutput) -> Vec<String> {
    output.render(None)["warnings"]
        .as_array()
        .map(|texts| {
            texts
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// An engine error response.
fn mock_failure(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(mock_error_body(status, message))
}

/// A mock engine knowing `definition` as `kind`.
async fn mock_engine_for(kind: &str, definition: Value) -> MockEngine {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", format!("spec.names.kind={kind}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![definition], None)),
        )
        .mount(engine.server())
        .await;

    engine
}

/// A mock engine knowing `definition` as `Service`.
async fn mock_engine_with(definition: Value) -> MockEngine {
    mock_engine_for("Service", definition).await
}

/// A mock engine knowing the fixture type, recording on.
async fn mock_engine() -> MockEngine {
    mock_engine_with(mock_item_type_definition("Service", "services", GROUP)).await
}

/// The fixture item before the restore: `resourceVersion` `1`.
fn mock_before() -> Value {
    mock_item("example-item")
}

/// The fixture item after a restore that set `spec.replicas` to 3, as the engine answers it.
fn mock_after() -> Value {
    let mut after = mock_before();
    after["spec"]["replicas"] = json!(3);
    after["metadata"]["updateTimestamp"] = json!(RESTORED_AT);
    after["resourceVersion"] = json!("2");
    after
}

/// Answers the pre-read with `before` and the restore with `after`.
async fn mount_restore(engine: &MockEngine, before: Value, after: Value) {
    engine.get_ok(ITEM_PATH, before).await;
    Mock::given(method("POST"))
        .and(path(RESTORE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(after))
        .mount(engine.server())
        .await;
}

/// Answers the pre-read with the fixture item and the restore with `response`.
async fn mount_refused_restore(engine: &MockEngine, response: ResponseTemplate) {
    engine.get_ok(ITEM_PATH, mock_before()).await;
    Mock::given(method("POST"))
        .and(path(RESTORE_PATH))
        .respond_with(response)
        .mount(engine.server())
        .await;
}

/// Answers the revision listing with `revisions`, newest first.
async fn mount_revisions(engine: &MockEngine, revisions: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(REVISIONS_PATH))
        .and(query_param("limit", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(revisions, None)))
        .mount(engine.server())
        .await;
}

/// The history a restore leaves: its own revision on top of `BELOW`.
async fn mount_history(engine: &MockEngine) {
    mount_revisions(
        engine,
        vec![
            mock_item_revision(OWN, RESTORED_AT, Some("1.4.0")),
            mock_item_revision(BELOW, EARLIER, None),
        ],
    )
    .await;
}

/// Answers the read of revision `BELOW` with `manifest`.
async fn mount_below(engine: &MockEngine, manifest: Value) {
    engine
        .get_ok(&format!("{REVISIONS_PATH}/{BELOW}"), manifest)
        .await;
}

/// `manifest` as a revision renders it: no `resourceVersion`, its own time.
fn as_revision(mut manifest: Value) -> Value {
    if let Value::Object(object) = &mut manifest {
        object.remove("resourceVersion");
    }
    manifest["metadata"]["updateTimestamp"] = json!(EARLIER);
    manifest
}

/// Every request of `verb` on `url_path`.
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

/// Every request the engine received.
async fn all_requests(engine: &MockEngine) -> Vec<wiremock::Request> {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
}

/// The error's details.
fn details(error: &ToolError) -> Value {
    error.details.as_deref().cloned().unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// The restore.
// ---------------------------------------------------------------------------------------------

/// No body, ever, and the pre-read's `resourceVersion` in the query; what changed, and the state
/// before — checked — named for the undo.
#[rstest]
#[tokio::test]
async fn test_a_restore_is_sent_bodiless_and_guarded() {
    let engine = mock_engine().await;
    mount_restore(&engine, mock_before(), mock_after()).await;
    mount_history(&engine).await;
    mount_below(&engine, as_revision(mock_before())).await;

    let output = run_with(&engine, mock_arguments()).await.expect("restored");

    let restores = requests_on(&engine, Method::POST, RESTORE_PATH).await;
    assert_eq!(restores.len(), 1);
    assert!(restores[0].body.is_empty(), "a restore carries no body");
    assert_eq!(restores[0].url.query(), Some("resourceVersion=1"));

    assert_eq!(
        output.payload(),
        &json!({
            "restored": true, "name": "example-item", "kind": "Service", "group": GROUP,
            "version": "v1", "family": "services", "versionName": "1.4.0",
            "changed": ["spec.replicas"], "noOp": false, "previousRevision": BELOW
        })
    );
    assert_eq!(warnings(&output), vec![CAMPAIGNS_WARNING.to_string()]);
}

/// `changed` is in `apply_item`'s format — dotted, sorted, arrays whole — over what a restore
/// writes, and never names identity or the server's own fields.
#[rstest]
#[case::spec("/spec/replicas", json!(5), &["spec.replicas"])]
#[case::label("/metadata/labels/environment", json!("prod"), &["metadata.labels.environment"])]
#[case::tags_whole("/metadata/tags", json!(["api", "new"]), &["metadata.tags"])]
#[case::owner(
    "/metadata/owner",
    json!({ "type": "principal", "ref": CALLER }),
    &["metadata.owner"]
)]
#[case::custom_field("/customFields", json!({ "sensitivity": "high" }), &["customFields"])]
#[case::empty_labels("/metadata/labels", json!({}), &["metadata.labels.environment"])]
#[case::null_title("/metadata/title", Value::Null, &["metadata.title"])]
#[case::uid("/metadata/uid", json!("another"), &[])]
#[case::updated("/metadata/updateTimestamp", json!("2027-01-01T00:00:00Z"), &[])]
#[case::resource_version("/resourceVersion", json!("9"), &[])]
#[case::urn("/metadata/urn", json!("urn:another"), &[])]
#[tokio::test]
async fn test_changed_paths(
    #[case] pointer: &str,
    #[case] value: Value,
    #[case] expected: &[&str],
) {
    let engine = mock_engine().await;
    let mut after = mock_before();
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer");
    let parent = if parent.is_empty() {
        &mut after
    } else {
        after.pointer_mut(parent).expect("the parent exists")
    };
    parent[key] = value;
    mount_restore(&engine, mock_before(), after).await;
    mount_revisions(&engine, vec![]).await;

    let payload = run(&engine, mock_arguments()).await.expect("restored");

    assert_eq!(payload["changed"], json!(expected));
    assert_eq!(payload["noOp"], json!(expected.is_empty()));
}

/// A restore that changed custom fields, the owner or the followers says so; one that changed only
/// the rest does not.
#[rstest]
#[case::custom_field(&["/customFields/sensitivity"], Some("custom fields"))]
#[case::owner_and_followers(
    &["/metadata/owner", "/metadata/followers"],
    Some("the owner and the followers")
)]
#[case::all_three(
    &["/customFields/sensitivity", "/metadata/owner", "/metadata/followers"],
    Some("custom fields, the owner, and the followers")
)]
#[case::spec_only(&["/spec/replicas"], None)]
#[tokio::test]
async fn test_the_note(#[case] pointers: &[&str], #[case] note: Option<&str>) {
    let engine = mock_engine().await;
    let mut before = mock_before();
    before["customFields"] = json!({ "sensitivity": "low" });
    let mut after = before.clone();
    for pointer in pointers {
        let (parent, key) = pointer.rsplit_once('/').expect("a pointer");
        let parent = after.pointer_mut(parent).expect("the parent exists");
        parent[key] = json!([{ "type": "principal", "ref": CALLER }]);
    }
    mount_restore(&engine, before, after).await;
    mount_revisions(&engine, vec![]).await;

    let payload = run(&engine, mock_arguments()).await.expect("restored");

    match note {
        Some(words) => assert_eq!(
            payload["note"],
            json!(format!("This restore also changed {words}."))
        ),
        None => assert!(payload.get("note").is_none(), "{payload}"),
    }
}

// ---------------------------------------------------------------------------------------------
// A restore that changes nothing.
// ---------------------------------------------------------------------------------------------

/// Sent, as the engine allows; reported as content unchanged, with no state before to name.
#[rstest]
#[tokio::test]
async fn test_a_no_op_is_sent_and_said() {
    let engine = mock_engine().await;
    let mut after = mock_before();
    after["metadata"]["updateTimestamp"] = json!(RESTORED_AT);
    after["resourceVersion"] = json!("2");
    mount_restore(&engine, mock_before(), after).await;
    mount_revisions(&engine, vec![mock_item_revision(BELOW, EARLIER, None)]).await;

    let output = run_with(&engine, mock_arguments()).await.expect("restored");

    let payload = output.payload();
    assert_eq!(payload["changed"], json!([]));
    assert_eq!(payload["noOp"], json!(true));
    assert_eq!(payload["note"], json!(NO_OP_NOTE));
    assert!(payload.get("previousRevision").is_none());
    assert_eq!(warnings(&output), vec![CAMPAIGNS_WARNING.to_string()]);
    assert_eq!(
        requests_on(&engine, Method::POST, RESTORE_PATH).await.len(),
        1
    );
    assert!(
        requests_on(&engine, Method::GET, &format!("{REVISIONS_PATH}/{BELOW}"))
            .await
            .is_empty()
    );
}

/// A no-op on top of a restore of the same version: an earlier one landed, and is named.
#[rstest]
#[tokio::test]
async fn test_a_no_op_after_an_earlier_restore_names_it() {
    let engine = mock_engine().await;
    mount_restore(&engine, mock_before(), mock_before()).await;
    mount_history(&engine).await;

    let output = run_with(&engine, mock_arguments()).await.expect("restored");

    assert!(output.payload().get("previousRevision").is_none());
    let warnings = warnings(&output);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[1].contains(OWN) && warnings[1].contains("already a restore"));
}

/// A no-op where history had a gap: the reconstruction on top is no restore, and nothing is said.
#[rstest]
#[tokio::test]
async fn test_a_no_op_with_a_reconstruction_on_top() {
    let engine = mock_engine().await;
    mount_restore(&engine, mock_before(), mock_before()).await;
    let mut resumption = mock_item_revision(OWN, RESTORED_AT, None);
    resumption["spec"]["origin"] = json!("Resumption");
    mount_revisions(&engine, vec![resumption]).await;

    let output = run_with(&engine, mock_arguments()).await.expect("restored");

    assert_eq!(output.payload()["noOp"], json!(true));
    assert_eq!(warnings(&output), vec![CAMPAIGNS_WARNING.to_string()]);
}

// ---------------------------------------------------------------------------------------------
// The state before.
// ---------------------------------------------------------------------------------------------

/// `previousRevision` is named only when checked to be the state before; otherwise the warning
/// says why it is not.
#[rstest]
#[case::retention_kept_one(
    vec![mock_item_revision(OWN, RESTORED_AT, Some("1.4.0"))],
    None,
    TRIMMED_WARNING
)]
#[case::below_differs(
    vec![
        mock_item_revision(OWN, RESTORED_AT, Some("1.4.0")),
        mock_item_revision(BELOW, EARLIER, None),
    ],
    Some(as_revision(mock_after())),
    TRIMMED_WARNING
)]
#[case::another_write_on_top(
    vec![
        mock_item_revision(BELOW, RESTORED_AT, None),
        mock_item_revision(OWN, RESTORED_AT, Some("1.4.0")),
    ],
    None,
    OVERTAKEN_WARNING
)]
#[case::an_older_restore_on_top(
    vec![
        mock_item_revision(OWN, EARLIER, Some("1.4.0")),
        mock_item_revision(BELOW, EARLIER, None),
    ],
    None,
    OVERTAKEN_WARNING
)]
#[case::another_versions_restore_on_top(
    vec![
        mock_item_revision(OWN, RESTORED_AT, Some("1.3.0")),
        mock_item_revision(BELOW, EARLIER, None),
    ],
    None,
    OVERTAKEN_WARNING
)]
#[tokio::test]
async fn test_the_state_before_is_named_only_when_checked(
    #[case] revisions: Vec<Value>,
    #[case] below: Option<Value>,
    #[case] warning: &str,
) {
    let engine = mock_engine().await;
    mount_restore(&engine, mock_before(), mock_after()).await;
    mount_revisions(&engine, revisions).await;
    if let Some(below) = below {
        mount_below(&engine, below).await;
    }

    let output = run_with(&engine, mock_arguments()).await.expect("restored");

    assert!(output.payload().get("previousRevision").is_none());
    assert_eq!(
        warnings(&output),
        vec![CAMPAIGNS_WARNING.to_string(), warning.to_string()]
    );
}

/// A history that cannot be read after the restore never fails it.
#[rstest]
#[case::listing_fails(true, false)]
#[case::revision_fails(false, true)]
#[tokio::test]
async fn test_unread_history_never_fails_the_restore(
    #[case] listing_fails: bool,
    #[case] revision_fails: bool,
) {
    let engine = mock_engine().await;
    mount_restore(&engine, mock_before(), mock_after()).await;
    if listing_fails {
        Mock::given(method("GET"))
            .and(path(REVISIONS_PATH))
            .respond_with(mock_failure(503, "unavailable"))
            .mount(engine.server())
            .await;
    } else {
        mount_history(&engine).await;
    }
    if revision_fails {
        Mock::given(method("GET"))
            .and(path(format!("{REVISIONS_PATH}/{BELOW}")))
            .respond_with(mock_failure(503, "unavailable"))
            .mount(engine.server())
            .await;
    }

    let output = run_with(&engine, mock_arguments()).await.expect("restored");

    assert_eq!(output.payload()["restored"], json!(true));
    assert_eq!(
        warnings(&output),
        vec![CAMPAIGNS_WARNING.to_string(), UNREAD_WARNING.to_string()]
    );
}

/// A deadline that runs out before the history is read: the restore still succeeds.
#[rstest]
#[tokio::test]
async fn test_a_late_history_read_never_fails_the_restore() {
    let engine = mock_engine().await;
    mount_restore(&engine, mock_before(), mock_after()).await;
    Mock::given(method("GET"))
        .and(path(REVISIONS_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_list_envelope(vec![], None))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(engine.server())
        .await;

    let output = run_within(&engine, mock_arguments(), Duration::from_secs(1))
        .await
        .expect("restored");

    assert_eq!(output.payload()["changed"], json!(["spec.replicas"]));
    assert_eq!(
        warnings(&output),
        vec![CAMPAIGNS_WARNING.to_string(), UNREAD_WARNING.to_string()]
    );
}

// ---------------------------------------------------------------------------------------------
// Refused before any call.
// ---------------------------------------------------------------------------------------------

/// A Campaign is refused first — even with recording off — with nothing sent beyond the lookup.
#[rstest]
#[case::recording_on(true)]
#[case::recording_off(false)]
#[tokio::test]
async fn test_a_campaign_is_refused_first(#[case] recording: bool) {
    let mut definition =
        mock_item_type_definition("Campaign", "campaigns", "compliance.mia-platform.eu");
    if !recording {
        definition["spec"]
            .as_object_mut()
            .expect("an object")
            .remove("history");
    }
    let engine = mock_engine_for("Campaign", definition).await;

    let error = run(
        &engine,
        json!({ "name": "q4-audit", "kind": "Campaign", "versionName": "1.0.0" }),
    )
    .await
    .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate)
    );
    assert_eq!(details(&error)["permanent"], json!(true));
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("apply_item"))
    );
    assert_eq!(all_requests(&engine).await.len(), 1);
}

/// Recording off, or one of the four kinds: refused with nothing sent beyond the lookup.
#[rstest]
#[case::recording_off("Service", "services", GROUP, false)]
#[case::relationship("Relationship", "relationships", "mia-platform.eu", true)]
#[tokio::test]
async fn test_what_cannot_record_is_refused_before_any_call(
    #[case] kind: &str,
    #[case] family: &str,
    #[case] group: &str,
    #[case] permanent: bool,
) {
    let mut definition = mock_item_type_definition(kind, family, group);
    if !permanent {
        definition["spec"]
            .as_object_mut()
            .expect("an object")
            .remove("history");
    }
    let engine = mock_engine_for(kind, definition).await;

    let error = run(
        &engine,
        json!({ "name": "example-item", "kind": kind, "versionName": "1.0.0" }),
    )
    .await
    .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate)
    );
    assert_eq!(details(&error)["permanent"], json!(permanent));
    assert_eq!(all_requests(&engine).await.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// The engine's refusals.
// ---------------------------------------------------------------------------------------------

/// The pre-read's `404`: no such item, and no version to suggest; nothing is restored.
#[rstest]
#[tokio::test]
async fn test_a_missing_item_is_no_undelete() {
    let engine = mock_engine().await;
    engine
        .get_error(ITEM_PATH, 404, "Item of family 'services' not found")
        .await;

    let error = run(&engine, mock_arguments()).await.expect_err("missing");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains("versions are gone"));
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("describe_item"))
    );
    assert!(
        requests_on(&engine, Method::POST, RESTORE_PATH)
            .await
            .is_empty()
    );
    assert!(
        requests_on(&engine, Method::GET, VERSIONS_PATH)
            .await
            .is_empty()
    );
}

/// The restore's `404` is the version: the item's versions are listed, every page.
#[rstest]
#[tokio::test]
async fn test_a_missing_version_lists_the_others() {
    let engine = mock_engine().await;
    mount_refused_restore(
        &engine,
        mock_failure(404, "Version '1.4.0' of item not found"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(VERSIONS_PATH))
        .and(query_param("continue", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_version("1.0.0", BELOW, CALLER)],
            None,
        )))
        .with_priority(1)
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(VERSIONS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![mock_item_version("2.0.0", OWN, CALLER)],
            Some("page-2"),
        )))
        .mount(engine.server())
        .await;

    let error = run(&engine, mock_arguments()).await.expect_err("missing");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        details(&error),
        json!({ "availableVersions": ["2.0.0", "1.0.0"], "availableCount": 2 })
    );
}

/// No versions at all; a type that vanished; a listing that cannot be read.
#[rstest]
#[case::no_versions("Version '1.4.0' of item not found", Some(200), "tag_item_version")]
#[case::the_type("Resource of family 'services' not found", None, "Resource of family")]
#[case::listing_fails("Version '1.4.0' of item not found", Some(404), "may be gone")]
#[tokio::test]
async fn test_a_restore_not_found_is_told_apart(
    #[case] message: &str,
    #[case] listing: Option<u16>,
    #[case] says: &str,
) {
    let engine = mock_engine().await;
    mount_refused_restore(&engine, mock_failure(404, message)).await;
    match listing {
        Some(200) => {
            Mock::given(method("GET"))
                .and(path(VERSIONS_PATH))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)),
                )
                .mount(engine.server())
                .await;
        }
        Some(status) => engine.get_error(VERSIONS_PATH, status, "gone").await,
        None => {}
    }

    let error = run(&engine, mock_arguments()).await.expect_err("missing");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    let text = format!("{} {}", error.message, error.next_step.unwrap_or_default());
    assert!(text.contains(says), "{text}");
    if listing.is_none() {
        assert!(
            requests_on(&engine, Method::GET, VERSIONS_PATH)
                .await
                .is_empty()
        );
    }
}

/// A `400` says why, by its source, and keeps the engine's words; another is relayed.
#[rstest]
#[case::schema(
    "Body does not conform to Item Type Definition schema: /spec/replicas",
    Some("get_item_schema")
)]
#[case::spec("failed to deserialize spec: missing field", Some("get_item_schema"))]
#[case::rule("Rule is invalid: bad query", Some("get_item_schema"))]
#[case::named_rule("Rule \"r1\" is invalid: bad query", Some("get_item_schema"))]
#[case::custom_key(
    "Restored custom field key 'sensitivity' is not registered or not applicable to this item type",
    Some("patch_item_custom_fields")
)]
#[case::custom_value(
    "Restored value for custom field 'sensitivity' is invalid: not one of the allowed values",
    Some("patch_item_custom_fields")
)]
#[case::other("Invalid header", None)]
#[tokio::test]
async fn test_a_bad_request_says_why(#[case] message: &str, #[case] step: Option<&str>) {
    let engine = mock_engine().await;
    mount_refused_restore(&engine, mock_failure(400, message)).await;

    let error = run(&engine, mock_arguments()).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    match step {
        Some(step) => {
            assert_eq!(details(&error)["engineMessage"], json!(message));
            assert!(error.next_step.is_some_and(|next| next.contains(step)));
        }
        None => assert_eq!(error.message, message),
    }
}

/// A `409` is reported once, never retried.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_reported_never_retried() {
    let engine = mock_engine().await;
    mount_refused_restore(&engine, mock_failure(409, "Concurrent modification")).await;

    let error = run(&engine, mock_arguments()).await.expect_err("conflict");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CONFLICT, Remedy::RetryLater)
    );
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("describe_item"))
    );
    assert_eq!(
        requests_on(&engine, Method::POST, RESTORE_PATH).await.len(),
        1
    );
}

/// The engine's `422`, when it arrives anyway, is classified by its phrase, or relayed.
#[rstest]
#[case::not_recorded(
    "Revision history is not being recorded for items of family 'services'",
    Some(false)
)]
#[case::other("Something else entirely", None)]
#[tokio::test]
async fn test_an_engine_refusal_is_classified(
    #[case] message: &str,
    #[case] permanent: Option<bool>,
) {
    let engine = mock_engine().await;
    mount_refused_restore(&engine, mock_failure(422, message)).await;

    let error = run(&engine, mock_arguments()).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate)
    );
    match permanent {
        Some(permanent) => assert_eq!(details(&error)["permanent"], json!(permanent)),
        None => assert_eq!(error.message, message),
    }
}

/// The gateway's `403` is explained from the item, with this tool's verb.
#[rstest]
#[case::ai_foundry(
    "ai.mia-platform.eu",
    "example-item",
    "restored only from the AI Foundry"
)]
#[case::built_in(GROUP, "catalog.mia-platform.eu", "cannot be restored")]
#[case::other(GROUP, "example-item", "not permitted")]
#[tokio::test]
async fn test_a_forbidden_restore_is_explained(
    #[case] group: &str,
    #[case] name: &str,
    #[case] says: &str,
) {
    let engine = mock_engine_with(mock_item_type_definition("Service", "services", group)).await;
    let mut item = mock_before();
    item["metadata"]["name"] = json!(name);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .with_priority(10)
        .mount(engine.server())
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403))
        .mount(engine.server())
        .await;

    let error = run(
        &engine,
        json!({ "name": name, "kind": "Service", "versionName": "1.4.0" }),
    )
    .await
    .expect_err("forbidden");

    assert_eq!(
        (error.code, error.remedy),
        (codes::FORBIDDEN, Remedy::Escalate)
    );
    assert!(error.message.contains(says), "{}", error.message);
}

/// A restore whose answer was lost may have landed: one request, and how to tell.
#[rstest]
#[tokio::test]
async fn test_a_lost_answer_is_an_unknown_outcome() {
    let engine = mock_engine().await;
    mount_refused_restore(&engine, ResponseTemplate::new(503)).await;

    let error = run(&engine, mock_arguments()).await.expect_err("unknown");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNKNOWN_OUTCOME, Remedy::Unknown)
    );
    assert!(
        error
            .next_step
            .is_some_and(|step| step.contains("describe_item") && step.contains("re-reads"))
    );
    assert_eq!(
        requests_on(&engine, Method::POST, RESTORE_PATH).await.len(),
        1
    );
}

/// A read failing before the restore: nothing was written.
#[rstest]
#[tokio::test]
async fn test_a_failed_pre_read_writes_nothing() {
    let engine = mock_engine().await;
    engine.get_error(ITEM_PATH, 503, "unavailable").await;

    let error = run(&engine, mock_arguments())
        .await
        .expect_err("unavailable");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
    assert!(
        requests_on(&engine, Method::POST, RESTORE_PATH)
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------------------------
// Bounds and words.
// ---------------------------------------------------------------------------------------------

/// The input bounds: refused before anything is sent, naming the parameter.
#[rstest]
#[case::api_version(json!({ "name": "example-item", "kind": "Service", "versionName": "v1" }), "versionName")]
#[case::not_semver(json!({ "name": "example-item", "kind": "Service", "versionName": "latest" }), "versionName")]
#[case::bad_name(json!({ "name": "Not A Name", "kind": "Service", "versionName": "1.0.0" }), "name")]
#[case::bad_kind(json!({ "name": "example-item", "kind": "9lives", "versionName": "1.0.0" }), "kind")]
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
    assert!(all_requests(&engine).await.is_empty());
}

/// The tool that replaces and the tools that keep are never described in each other's words.
#[rstest]
fn test_the_descriptions_never_share_their_promises() {
    let restore = RestoreItemVersion::descriptor().description;
    let keeping = [
        ApplyItem::descriptor().description,
        PatchItemCustomFields::descriptor().description,
    ];

    assert!(restore.contains("anything changed since is lost"));
    assert!(!restore.contains("left out is kept"));
    assert!(!restore.contains("keys left out are kept"));
    for description in keeping {
        assert!(!description.contains("anything changed since is lost"));
    }
}

/// No hint is emitted: destructive by default, and not idempotent.
#[rstest]
fn test_no_hint_is_emitted() {
    let annotations = RestoreItemVersion::descriptor().annotations;

    assert_eq!(annotations.destructive_hint, None);
    assert_eq!(annotations.idempotent_hint, None);
    assert_eq!(annotations.read_only_hint, None);
}
