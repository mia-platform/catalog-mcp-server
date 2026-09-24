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
    error::codes,
    testing::{MOCK_ITEM_NAME, MockEngine, mock_identity, mock_item},
    write::{
        ConflictPolicy, ResourceVersionIn, WriteCycle, changed_paths, merge_patch,
        strip_server_owned,
    },
};
use rstest::rstest;
use serde_json::{Value, json};
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path_regex},
};

// ---------------------------------------------------------------------------------------------
// RFC 7396 Appendix A, transcribed. The RFC's own table is the test list (§8.5, D29).
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::replace_a_value(json!({"a": "b"}), json!({"a": "c"}), json!({"a": "c"}))]
#[case::add_a_value(json!({"a": "b"}), json!({"b": "c"}), json!({"a": "b", "b": "c"}))]
#[case::delete_a_value(json!({"a": "b"}), json!({"a": null}), json!({}))]
#[case::delete_one_of_two(
    json!({"a": "b", "b": "c"}),
    json!({"a": null}),
    json!({"b": "c"})
)]
#[case::replace_an_array(json!({"a": ["b"]}), json!({"a": "c"}), json!({"a": "c"}))]
#[case::replace_with_an_array(json!({"a": "c"}), json!({"a": ["b"]}), json!({"a": ["b"]}))]
#[case::recurse_into_an_object(
    json!({"a": {"b": "c"}}),
    json!({"a": {"b": "d", "c": null}}),
    json!({"a": {"b": "d"}})
)]
#[case::replace_an_array_of_objects(
    json!({"a": [{"b": "c"}]}),
    json!({"a": [1]}),
    json!({"a": [1]})
)]
#[case::replace_an_array_wholesale(json!(["a", "b"]), json!(["c", "d"]), json!(["c", "d"]))]
#[case::object_replaced_by_array(json!({"a": "b"}), json!(["c"]), json!(["c"]))]
#[case::object_replaced_by_null(json!({"a": "foo"}), json!(null), json!(null))]
#[case::object_replaced_by_string(json!({"a": "foo"}), json!("bar"), json!("bar"))]
#[case::null_member_is_not_added(json!({"e": null}), json!({"a": 1}), json!({"e": null, "a": 1}))]
#[case::array_replaced_by_object(json!([1, 2]), json!({"a": "b", "c": null}), json!({"a": "b"}))]
#[case::deep_delete(json!({}), json!({"a": {"bb": {"ccc": null}}}), json!({"a": {"bb": {}}}))]
fn test_rfc_7396_appendix_a(
    #[case] mut target: Value,
    #[case] patch: Value,
    #[case] expected: Value,
) {
    merge_patch(&mut target, &patch);

    assert_eq!(target, expected);
}

/// The property the RFC's table is really about: a `null` deletes rather than storing a null.
#[rstest]
fn test_null_deletes_rather_than_storing_null() {
    let mut target = json!({ "metadata": { "title": "Old", "description": "Gone" } });

    merge_patch(&mut target, &json!({ "metadata": { "description": null } }));

    assert_eq!(target, json!({ "metadata": { "title": "Old" } }));
    assert!(
        !target["metadata"]
            .as_object()
            .unwrap()
            .contains_key("description")
    );
}

/// **There is no way to append to a list with a merge patch**, which is why `apply_item`
/// describes it to the model as "send only what you want to change".
#[rstest]
fn test_arrays_are_replaced_never_merged() {
    let mut target = json!({ "metadata": { "tags": ["api", "public"] } });

    merge_patch(
        &mut target,
        &json!({ "metadata": { "tags": ["internal"] } }),
    );

    assert_eq!(target["metadata"]["tags"], json!(["internal"]));
}

// ---------------------------------------------------------------------------------------------
// Stripping the fields a `PUT` ignores.
// ---------------------------------------------------------------------------------------------

/// Echoing `customFields` back would tell the model a write happened that did not: the engine
/// ignores the field on `PUT` and says so in a `Warning`.
#[rstest]
fn test_custom_fields_are_stripped() {
    let mut manifest = json!({
        "apiVersion": "stable.example.com/v1",
        "spec": {},
        "customFields": { "costCentre": "abc" },
    });

    strip_server_owned(&mut manifest);

    assert!(manifest.get("customFields").is_none());
    assert!(manifest.get("spec").is_some());
}

#[rstest]
fn test_read_only_metadata_is_stripped() {
    let mut manifest = json!({
        "metadata": {
            "name": "example-item",
            "title": "Kept",
            "uid": "550e8400-e29b-41d4-a716-446655440000",
            "urn": "urn:mia-platform-catalog:x:v1:Service:example-item",
            "creationTimestamp": "2026-09-17T10:30:45Z",
            "updateTimestamp": "2026-09-17T10:30:45Z",
            "family": "services",
        },
    });

    strip_server_owned(&mut manifest);

    let metadata = manifest["metadata"].as_object().expect("an object");

    assert!(metadata.contains_key("name"));
    assert!(metadata.contains_key("title"));
    for stripped in [
        "uid",
        "urn",
        "creationTimestamp",
        "updateTimestamp",
        "family",
    ] {
        assert!(!metadata.contains_key(stripped), "{stripped} survived");
    }
}

// ---------------------------------------------------------------------------------------------
// The diff, which is what lets a tool report a no-op honestly.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_an_unchanged_manifest_reports_nothing() {
    let manifest = json!({ "metadata": { "title": "Same" }, "spec": { "replicas": 2 } });

    assert!(changed_paths(&manifest, &manifest).is_empty());
}

#[rstest]
fn test_changed_paths_are_dotted_and_ordered() {
    let before = json!({ "metadata": { "title": "Old" }, "spec": { "replicas": 2 } });
    let after = json!({ "metadata": { "title": "New" }, "spec": { "replicas": 3 } });

    assert_eq!(
        changed_paths(&before, &after),
        vec!["metadata.title", "spec.replicas"]
    );
}

#[rstest]
fn test_an_added_field_is_reported() {
    let before = json!({ "metadata": {} });
    let after = json!({ "metadata": { "title": "New" } });

    assert_eq!(changed_paths(&before, &after), vec!["metadata.title"]);
}

#[rstest]
fn test_a_removed_field_is_reported() {
    let before = json!({ "metadata": { "title": "Gone" } });
    let after = json!({ "metadata": {} });

    assert_eq!(changed_paths(&before, &after), vec!["metadata.title"]);
}

/// An array changes as a whole, because a merge patch can only replace it as a whole —
/// reporting an element index would describe an operation the caller cannot express.
#[rstest]
fn test_an_array_change_is_reported_at_the_array() {
    let before = json!({ "metadata": { "tags": ["a", "b"] } });
    let after = json!({ "metadata": { "tags": ["a", "c"] } });

    assert_eq!(changed_paths(&before, &after), vec!["metadata.tags"]);
}

// ---------------------------------------------------------------------------------------------
// The cycle, against the mock engine.
// ---------------------------------------------------------------------------------------------

fn mock_address() -> ItemAddress {
    ItemAddress::new("stable.example.com", "v1", "services", MOCK_ITEM_NAME)
        .expect("a well-formed fixture address")
}

/// The item path, for a mock that must distinguish `GET` from `PUT`.
const ITEM_PATH: &str = r"^/stable\.example\.com/v1/items/services/.*$";

#[rstest]
#[tokio::test]
async fn test_a_patch_is_merged_onto_the_current_state() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    let mut written = mock_item(MOCK_ITEM_NAME);
    written["metadata"]["title"] = json!("Renamed");

    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(written))
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    let cycle = WriteCycle::new(&client, ConflictPolicy::RetryOnce, ResourceVersionIn::Body);

    let outcome = cycle
        .apply(
            &mock_address(),
            &json!({ "metadata": { "title": "Renamed" } }),
        )
        .await
        .expect("the write succeeds");

    assert!(!outcome.created);
    assert!(!outcome.retried);
    assert_eq!(outcome.changed, vec!["metadata.title"]);
    assert!(!outcome.is_noop());
}

/// A patch that changes nothing says so, rather than claiming a write.
#[rstest]
#[tokio::test]
async fn test_a_no_op_write_is_reported_as_one() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    let cycle = WriteCycle::new(&client, ConflictPolicy::RetryOnce, ResourceVersionIn::Body);

    let outcome = cycle
        .apply(
            &mock_address(),
            &json!({ "metadata": { "title": "Example Service" } }),
        )
        .await
        .expect("the write succeeds");

    assert!(outcome.is_noop());
    assert!(outcome.changed.is_empty());
}

/// A `404` on the read means create, not fail.
#[rstest]
#[tokio::test]
async fn test_a_missing_object_is_created() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(crate::testing::mock_error_body(404, "not found")),
        )
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    let cycle = WriteCycle::new(&client, ConflictPolicy::RetryOnce, ResourceVersionIn::Body);

    let outcome = cycle
        .apply(&mock_address(), &json!({ "spec": { "replicas": 2 } }))
        .await
        .expect("a create succeeds");

    assert!(outcome.created);
}

/// D23 — `RetryOnce` re-reads and re-applies **once**, and says that it did.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_retried_once_under_retry_once() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(crate::testing::mock_error_body(409, "conflict")),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(engine.server())
        .await;

    let mut written = mock_item(MOCK_ITEM_NAME);
    written["metadata"]["title"] = json!("Renamed");
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(written))
        .expect(1)
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    let cycle = WriteCycle::new(&client, ConflictPolicy::RetryOnce, ResourceVersionIn::Body);

    let outcome = cycle
        .apply(
            &mock_address(),
            &json!({ "metadata": { "title": "Renamed" } }),
        )
        .await
        .expect("the re-applied write succeeds");

    assert!(outcome.retried, "a retry must be visible, never inferred");
    assert_eq!(outcome.changed, vec!["metadata.title"]);
}

/// D23 — `Report` does not retry. The caller's intent was formed against the state they read,
/// and landing it on a different one is not the same act.
#[rstest]
#[tokio::test]
async fn test_a_conflict_is_reported_under_report() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(crate::testing::mock_error_body(409, "conflict")),
        )
        .expect(1)
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    let cycle = WriteCycle::new(&client, ConflictPolicy::Report, ResourceVersionIn::Body);

    let error = cycle
        .apply(
            &mock_address(),
            &json!({ "metadata": { "title": "Renamed" } }),
        )
        .await
        .expect_err("a conflict under Report is an error");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, crate::Remedy::RetryLater);
}

/// The `resourceVersion` read from the current state goes into the **body** for an item write.
#[rstest]
#[tokio::test]
async fn test_the_resource_version_is_placed_in_the_body() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    WriteCycle::new(&client, ConflictPolicy::RetryOnce, ResourceVersionIn::Body)
        .apply(&mock_address(), &json!({ "spec": { "replicas": 3 } }))
        .await
        .expect("the write succeeds");

    let sent = sent_body(&engine).await;

    assert_eq!(sent["resourceVersion"], json!("1"));
    assert!(sent.get("customFields").is_none());
}

/// For custom fields and restore the token is a **query parameter**, so it must not be in the
/// body — the engine would reject the write outright.
#[rstest]
#[tokio::test]
async fn test_the_resource_version_is_kept_out_of_the_body_when_it_belongs_in_the_query() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());
    WriteCycle::new(&client, ConflictPolicy::Report, ResourceVersionIn::Query)
        .apply(&mock_address(), &json!({ "spec": { "replicas": 3 } }))
        .await
        .expect("the write succeeds");

    assert!(sent_body(&engine).await.get("resourceVersion").is_none());
}

/// D20 — a `500` on the write is `unknown_outcome`, never a clean failure.
#[rstest]
#[tokio::test]
async fn test_a_500_on_the_write_may_have_succeeded() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(crate::testing::mock_error_body(500, "Something went wrong")),
        )
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());

    let error = WriteCycle::new(&client, ConflictPolicy::Report, ResourceVersionIn::Body)
        .apply(&mock_address(), &json!({ "spec": { "replicas": 3 } }))
        .await
        .expect_err("a 500 on a write is an error");

    assert_eq!(error.code, codes::UNKNOWN_OUTCOME);
    assert_eq!(error.remedy, crate::Remedy::Unknown);
}

/// The warnings the write produced ride on the outcome, so a tool can surface them.
#[rstest]
#[tokio::test]
async fn test_write_warnings_reach_the_outcome() {
    let engine = MockEngine::start().await;

    Mock::given(method("GET"))
        .and(path_regex(ITEM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_item(MOCK_ITEM_NAME)))
        .mount(engine.server())
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(ITEM_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_item(MOCK_ITEM_NAME))
                .append_header(
                    "Warning",
                    r#"299 - "'spec.group' field is read-only and was ignored during the update.""#,
                ),
        )
        .mount(engine.server())
        .await;

    let client = engine.client(mock_identity());

    let outcome = WriteCycle::new(&client, ConflictPolicy::Report, ResourceVersionIn::Body)
        .apply(&mock_address(), &json!({ "spec": { "replicas": 3 } }))
        .await
        .expect("the write succeeds");

    assert_eq!(outcome.warnings.len(), 1);
    assert_eq!(
        outcome.warnings[0].read_only_field(),
        Some("spec.group"),
        "T12 derives its `ignored` list from exactly this"
    );
}

/// The body of the last `PUT` the mock received.
async fn sent_body(engine: &MockEngine) -> Value {
    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    let put = requests
        .iter()
        .rev()
        .find(|request| request.method == wiremock::http::Method::PUT)
        .expect("a PUT was sent");

    serde_json::from_slice(&put.body).expect("the write body is JSON")
}
