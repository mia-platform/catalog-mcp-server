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
    error::codes,
    models::TypeVersion,
    resolve::{resolve_kind, select_served_version},
    testing::{MockEngine, mock_identity, mock_item_type_definition, mock_list_envelope},
};
use rstest::rstest;
use serde_json::json;

/// A version, named and flagged.
fn version(name: &str, served: bool, deprecated: bool) -> TypeVersion {
    TypeVersion {
        name: name.to_string(),
        served,
        deprecated: Some(deprecated),
        schema: None,
        selectable_fields: vec![],
    }
}

/// The chosen version's name, or `None` when the type is unaddressable.
fn chosen(versions: &[TypeVersion]) -> Option<&str> {
    select_served_version(versions).map(|version| version.name.as_str())
}

// ---------------------------------------------------------------------------------------------
// The served-version rule (§8.6, from T1).
// ---------------------------------------------------------------------------------------------

/// The case that is true of all 68 shipped types today: one served `v1`.
#[rstest]
fn test_a_single_served_version_is_chosen() {
    assert_eq!(chosen(&[version("v1", true, false)]), Some("v1"));
}

/// Only `served: true` is considered — an unserved version cannot address anything.
#[rstest]
fn test_an_unserved_version_is_never_chosen() {
    assert_eq!(
        chosen(&[version("v2", false, false), version("v1", true, false)]),
        Some("v1")
    );
}

/// A non-deprecated version wins, even against a higher number.
#[rstest]
fn test_a_non_deprecated_version_beats_a_deprecated_higher_one() {
    assert_eq!(
        chosen(&[version("v2", true, true), version("v1", true, false)]),
        Some("v1")
    );
}

/// Then stability, then number: `v2` > `v1` > `v2beta1` > `v1alpha1`.
#[rstest]
fn test_the_stability_and_number_order() {
    let all = [
        version("v1alpha1", true, false),
        version("v2beta1", true, false),
        version("v1", true, false),
        version("v2", true, false),
    ];

    assert_eq!(chosen(&all), Some("v2"));
    assert_eq!(chosen(&all[..3]), Some("v1"));
    assert_eq!(chosen(&all[..2]), Some("v2beta1"));
    assert_eq!(chosen(&all[..1]), Some("v1alpha1"));
}

#[rstest]
fn test_a_higher_beta_beats_a_lower_one() {
    assert_eq!(
        chosen(&[
            version("v2beta1", true, false),
            version("v2beta2", true, false)
        ]),
        Some("v2beta2")
    );
}

#[rstest]
fn test_a_missing_deprecated_flag_counts_as_not_deprecated() {
    let versions = [TypeVersion {
        name: "v1".to_string(),
        served: true,
        deprecated: None,
        schema: None,
        selectable_fields: vec![],
    }];

    assert_eq!(chosen(&versions), Some("v1"));
}

/// A type with no served version at all is **not addressable**.
#[rstest]
fn test_no_served_version_selects_nothing() {
    assert_eq!(chosen(&[version("v1", false, false)]), None);
    assert_eq!(chosen(&[]), None);
}

// ---------------------------------------------------------------------------------------------
// The lookup itself.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_a_kind_resolves_to_its_coordinates() {
    let engine = MockEngine::start().await;
    engine
        .get_ok(
            "/mia-platform.eu/v1/item-type-definitions",
            mock_list_envelope(
                vec![mock_item_type_definition(
                    "Service",
                    "services",
                    "stable.example.com",
                )],
                None,
            ),
        )
        .await;

    let (coordinates, warnings) = resolve_kind(&engine.client(mock_identity()), "Service")
        .await
        .expect("the kind resolves");

    assert_eq!(coordinates.group, "stable.example.com");
    assert_eq!(coordinates.version, "v1");
    assert_eq!(coordinates.family, "services");
    assert_eq!(coordinates.kind, "Service");
    assert_eq!(coordinates.type_name, "services.stable.example.com");
    assert_eq!(coordinates.selectable_fields, vec!["spec.replicas"]);
    assert!(coordinates.history_enabled);
    assert!(warnings.is_empty());
}

/// **One request, and it is a point lookup** — `field=spec.names.kind=<kind>&limit=1`, tenant
/// scoped by construction (D30).
#[rstest]
#[tokio::test]
async fn test_the_lookup_is_one_tenant_scoped_point_query() {
    let engine = MockEngine::start().await;
    engine
        .get_ok(
            "/mia-platform.eu/v1/item-type-definitions",
            mock_list_envelope(
                vec![mock_item_type_definition(
                    "Service",
                    "services",
                    "stable.example.com",
                )],
                None,
            ),
        )
        .await;

    let _ = resolve_kind(&engine.client(mock_identity()), "Service").await;

    let requests = engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests");

    assert_eq!(requests.len(), 1, "resolution must be a single request");

    let pairs: Vec<(String, String)> = requests[0]
        .url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    // Two, not one: a second row is impossible, and asking for it is what makes it visible.
    assert!(pairs.contains(&("limit".to_string(), "2".to_string())));
    assert!(pairs.contains(&("field".to_string(), "spec.names.kind=Service".to_string())));
    // Tenancy comes from the forwarded header, not from a query parameter of ours.
    assert!(requests[0].headers.get("x-mia-acl-context").is_some());
}

/// `404`-loud rather than silently wrong: an unknown kind names itself and points somewhere.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_is_not_found_with_a_next_step() {
    let engine = MockEngine::start().await;
    engine
        .get_ok(
            "/mia-platform.eu/v1/item-type-definitions",
            mock_list_envelope(vec![], None),
        )
        .await;

    let error = resolve_kind(&engine.client(mock_identity()), "NoSuchKind")
        .await
        .expect_err("an unknown kind is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, crate::Remedy::RetryAfterChange);
    assert_eq!(
        error.details.expect("the kind is echoed")["kind"],
        json!("NoSuchKind")
    );
    assert_eq!(
        error.next_step.as_deref(),
        Some("call list_catalog_types to see the kinds that do exist")
    );
}

/// A kind that exists but serves nothing is `unaddressable_type` — and says which versions do
/// exist, so the answer is actionable by a human even though the model cannot fix it.
#[rstest]
#[tokio::test]
async fn test_a_type_with_no_served_version_is_unaddressable() {
    let engine = MockEngine::start().await;

    let mut definition = mock_item_type_definition("Service", "services", "stable.example.com");
    definition["spec"]["versions"][0]["served"] = json!(false);

    engine
        .get_ok(
            "/mia-platform.eu/v1/item-type-definitions",
            mock_list_envelope(vec![definition], None),
        )
        .await;

    let error = resolve_kind(&engine.client(mock_identity()), "Service")
        .await
        .expect_err("an unserved type is an error");

    assert_eq!(error.code, codes::UNADDRESSABLE_TYPE);
    assert_eq!(error.remedy, crate::Remedy::Escalate);

    let details = error.details.expect("the versions are reported");
    assert_eq!(details["versions"], json!(["v1"]));
    assert_eq!(details["servedVersions"], json!([]));
}

/// An engine failure during resolution is the engine's error, not a "kind not found".
#[rstest]
#[tokio::test]
async fn test_an_engine_failure_is_not_reported_as_a_missing_kind() {
    let engine = MockEngine::start().await;
    engine
        .get_error(
            "/mia-platform.eu/v1/item-type-definitions",
            503,
            "unavailable",
        )
        .await;

    let error = resolve_kind(&engine.client_without_retries(mock_identity()), "Service")
        .await
        .expect_err("an unavailable engine is an error");

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
}

/// T1-D4 — the lean listing model reaches **the same** rule, not a copy of it.
#[rstest]
fn test_the_lean_model_is_selected_by_the_same_rule() {
    let lean = [
        crate::models::ItdVersion {
            name: "v1".to_string(),
            served: true,
            deprecated: Some(true),
        },
        crate::models::ItdVersion {
            name: "v1beta1".to_string(),
            served: true,
            deprecated: None,
        },
        crate::models::ItdVersion {
            name: "v2".to_string(),
            served: false,
            deprecated: None,
        },
    ];

    assert_eq!(
        select_served_version(&lean).map(|version| version.name.as_str()),
        Some("v1beta1"),
        "a served, non-deprecated beta beats a deprecated stable, and an unserved v2 never wins"
    );
}

/// T6-D2 — two types claiming one `kind` break the catalog's own invariant: `server_defect`,
/// and **neither** is picked.
#[rstest]
#[tokio::test]
async fn test_two_types_with_one_kind_are_a_server_defect() {
    let engine = MockEngine::start().await;
    engine
        .get_ok(
            "/mia-platform.eu/v1/item-type-definitions",
            mock_list_envelope(
                vec![
                    mock_item_type_definition("Service", "services", "stable.example.com"),
                    mock_item_type_definition("Service", "services", "other.example.com"),
                ],
                None,
            ),
        )
        .await;

    let error = resolve_kind(&engine.client(mock_identity()), "Service")
        .await
        .expect_err("an ambiguous kind is never resolved");

    assert_eq!(error.code, codes::SERVER_DEFECT);
    assert_eq!(error.remedy, crate::error::Remedy::Escalate);
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["itemTypes"].clone()),
        Some(json!([
            "services.stable.example.com",
            "services.other.example.com"
        ]))
    );
}
