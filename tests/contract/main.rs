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
// Contract tests against the vendored engine OpenAPI document (§12.3).
//
// They answer one question: **would the requests this client builds still be understood by the
// engine it was written against?** Read with `serde_json` pointers and nothing else — no OpenAPI
// crate, which is a dependency for a job four pointer lookups do.
//
// Two things the OAS cannot cover, and which therefore need `cargo make e2e` against a live
// engine instead: the `Warning: 299` behaviour, which is declared nowhere, and the three
// response shapes of the relationships endpoint, whose `200` body is typed as a free-form
// object. Both are recorded as engine asks in §17.4.

use catalog_client::ops::{OPERATIONS, OperationSpec};
use serde_json::Value;
use std::sync::LazyLock;

/// The vendored document. Pinned and refreshed deliberately by `cargo make refresh_oas`, never
/// fetched at build time.
static OAS: LazyLock<Value> = LazyLock::new(|| {
    let raw = include_str!("../../assets/oas/catalog-engine.openapi.json");

    serde_json::from_str(raw).expect("the vendored OAS is valid JSON")
});

/// The operation object for one spec, or a failure naming what is missing.
fn operation(spec: &OperationSpec) -> &'static Value {
    let path = OAS
        .pointer(&format!("/paths/{}", escape(spec.path)))
        .unwrap_or_else(|| panic!("the engine no longer declares the path `{}`", spec.path));

    path.get(spec.method).unwrap_or_else(|| {
        panic!(
            "the engine no longer declares `{}` on `{}`",
            spec.method.to_uppercase(),
            spec.path
        )
    })
}

/// JSON-Pointer escaping: `~` becomes `~0` and `/` becomes `~1`.
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Every parameter an operation declares, as `(location, name)`.
fn declared_parameters(operation: &Value) -> Vec<(String, String)> {
    operation
        .get("parameters")
        .and_then(Value::as_array)
        .map(|parameters| {
            parameters
                .iter()
                .filter_map(|parameter| {
                    Some((
                        parameter.get("in")?.as_str()?.to_string(),
                        parameter.get("name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A named schema from `components/schemas`.
fn schema(name: &str) -> &'static Value {
    OAS.pointer(&format!("/components/schemas/{}", escape(name)))
        .unwrap_or_else(|| panic!("the engine no longer declares the schema `{name}`"))
}

/// Asserts that a schema declares every one of `fields` as a property.
fn assert_properties(schema_name: &str, fields: &[&str]) {
    let properties = schema(schema_name)
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("`{schema_name}` declares no properties"));

    for field in fields {
        assert!(
            properties.contains_key(*field),
            "this client reads `{field}` from `{schema_name}`, which no longer declares it"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// One test per client operation.
// ---------------------------------------------------------------------------------------------

/// Every operation's path and method still exist.
#[test]
fn test_every_operation_path_and_method_exist() {
    for spec in OPERATIONS {
        // Panics inside with a message naming the operation.
        let _ = operation(spec);
    }
}

/// Every query parameter this client may send is declared by the operation it is sent on.
///
/// This is the assertion that fails when the engine renames or drops a parameter, rather than
/// the model seeing an unexplained `400`.
#[test]
fn test_every_query_parameter_we_send_is_declared() {
    for spec in OPERATIONS {
        let declared = declared_parameters(operation(spec));

        for parameter in spec.query {
            assert!(
                declared
                    .iter()
                    .any(|(location, name)| location == "query" && name == parameter),
                "`{}` sends `{parameter}`, which `{}` no longer declares",
                spec.id,
                spec.path
            );
        }
    }
}

/// T11 — `/bff/tenants` still declares **no parameters** and still returns a bare array.
///
/// It is one of three routes that skip the policy, so it declares no `x-mia-acl-context` either;
/// that asymmetry is the reason the operation below is excluded from the identity-header sweep,
/// and the reason T11 proves *our* forwarding rather than the policy's regeneration.
#[test]
fn test_the_tenant_listing_declares_no_parameters_and_returns_a_bare_array() {
    let operation = operation(&catalog_client::ops::LIST_TENANTS);

    assert!(
        declared_parameters(operation).is_empty(),
        "`/bff/tenants` has grown parameters: {:?}",
        declared_parameters(operation)
    );

    let body = operation
        .pointer("/responses/200/content/application~1json/schema")
        .expect("the tenant listing still declares a 200 body");

    assert_eq!(
        body.get("type").and_then(Value::as_str),
        Some("array"),
        "`/bff/tenants` is no longer a bare array: {body}"
    );
    assert_eq!(
        body.pointer("/items/$ref").and_then(Value::as_str),
        Some("#/components/schemas/Tenant")
    );
}

/// The `401`-on-missing-token behaviour T11's whole purpose rests on.
#[test]
fn test_the_tenant_listing_still_declares_a_401() {
    let responses = operation(&catalog_client::ops::LIST_TENANTS)
        .get("responses")
        .and_then(Value::as_object)
        .expect("the tenant listing declares responses");

    assert!(
        responses.contains_key("401"),
        "the 401 T11 exists to surface is gone: {:?}",
        responses.keys().collect::<Vec<_>>()
    );
    assert!(
        responses.contains_key("502"),
        "the authz-unavailable row is gone"
    );
}

/// The fields T11 projects still exist, under the names that make `current` comparable to an
/// entry in the list: the engine's `name` is the slug and its `title` the display name.
#[test]
fn test_the_tenant_fields_we_read_still_exist() {
    assert_properties("Tenant", &["name", "organization", "title"]);
}

/// Every operation accepts the identity pair we forward. `x-mia-principal-id` is deliberately
/// **absent** from the OAS — it is an internal contract between the policy layer and the engine
/// — so only the ACL context is assertable here, and that asymmetry is the point of the comment
/// rather than a gap in the test.
#[test]
fn test_every_operation_declares_the_acl_context_header() {
    for spec in OPERATIONS {
        // The three `/bff/*` routes that skip the policy declare no parameters at all — see the
        // tenant-listing test above, which asserts that emptiness directly.
        if spec.path.starts_with("/bff/") {
            continue;
        }

        let declared = declared_parameters(operation(spec));

        assert!(
            declared
                .iter()
                .any(|(location, name)| location == "header" && name == "x-mia-acl-context"),
            "`{}` no longer declares `x-mia-acl-context`",
            spec.id
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The specific facts §12.3 names.
// ---------------------------------------------------------------------------------------------

/// `limit`'s maximum is still 200, which is what bounds a page and therefore a tool's default.
#[test]
fn test_the_limit_maximum_is_still_200() {
    let limit = declared_parameter_schema(&catalog_client::ops::LIST_ITEMS, "limit");

    assert_eq!(limit.get("maximum").and_then(Value::as_u64), Some(200));
    assert_eq!(
        limit.get("default").and_then(Value::as_u64),
        Some(u64::from(catalog_client::pagination::DEFAULT_LIMIT))
    );
    assert_eq!(
        catalog_client::pagination::MAX_LIMIT,
        200,
        "this client's MAX_LIMIT no longer matches the engine's"
    );
}

/// The `Accept` enum still contains the partial projection, which is what a search depends on.
#[test]
fn test_the_accept_enum_still_contains_the_partial_projection() {
    let accept = declared_parameter_schema(&catalog_client::ops::LIST_ITEMS, "Accept");

    let values: Vec<&str> = accept
        .get("enum")
        .and_then(Value::as_array)
        .expect("`Accept` declares an enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    assert!(values.contains(&catalog_client::Projection::Full.accept()));
    assert!(values.contains(&catalog_client::Projection::PartialObjectMetadata.accept()));
}

/// `field=spec.names.kind` is still filterable, which is the whole of the `kind → coordinates`
/// point lookup (§8.6).
#[test]
fn test_the_item_type_definition_listing_still_accepts_a_field_filter() {
    let declared = declared_parameters(operation(&catalog_client::ops::LIST_ITEM_TYPE_DEFINITIONS));

    assert!(
        declared
            .iter()
            .any(|(location, name)| location == "query" && name == "field"),
        "the item-type-definition listing no longer accepts `field`"
    );
}

/// `PATCH …/custom-fields` still requires `application/merge-patch+json`; sending anything else
/// is a `415`, which the contract maps to `server_defect` because the content type is ours.
#[test]
fn test_custom_fields_still_requires_merge_patch_json() {
    let content = OAS
        .pointer(&format!(
            "/paths/{}/patch/requestBody/content",
            escape("/{group}/{version}/items/{family}/{name}/custom-fields")
        ))
        .and_then(Value::as_object)
        .expect("the custom-fields patch still declares a request body");

    assert!(
        content.contains_key("application/merge-patch+json"),
        "custom-fields no longer requires `application/merge-patch+json`: {:?}",
        content.keys().collect::<Vec<_>>()
    );
}

/// `POST …/versions` still declares a `501`, which is the row T15's error mapping rests on
/// (P13, §17.4 item 5).
#[test]
fn test_version_creation_still_declares_a_501() {
    let responses = OAS
        .pointer(&format!(
            "/paths/{}/post/responses",
            escape("/{group}/{version}/items/{family}/{name}/versions")
        ))
        .and_then(Value::as_object)
        .expect("version creation still declares responses");

    assert!(
        responses.contains_key("501"),
        "the declared 501 on version creation is gone: {:?}",
        responses.keys().collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------------------------
// Every response field this client reads still exists.
// ---------------------------------------------------------------------------------------------

#[test]
fn test_the_item_fields_we_read_still_exist() {
    assert_properties(
        "miaplatform.eu.v1.Item",
        &[
            "apiVersion",
            "kind",
            "metadata",
            "spec",
            "customFields",
            "resourceVersion",
        ],
    );
}

#[test]
fn test_the_object_metadata_fields_we_read_still_exist() {
    assert_properties(
        "miaplatform.eu.v1.ObjectMetadata",
        &[
            "name",
            "family",
            "title",
            "description",
            "labels",
            "annotations",
            "tags",
            "links",
            "urn",
            "uid",
            "creationTimestamp",
            "updateTimestamp",
            "owner",
            "followers",
        ],
    );
}

/// The partial projection carries the **full** `ObjectMetadata` and no `spec` — which is the
/// property that makes it useful and the one a search depends on.
#[test]
fn test_the_partial_projection_still_has_no_spec_and_a_full_metadata() {
    let properties = schema("miaplatform.eu.v1.PartialObjectMetadata")
        .get("properties")
        .and_then(Value::as_object)
        .expect("the partial projection declares properties");

    assert!(!properties.contains_key("spec"));
    assert_eq!(
        properties
            .get("metadata")
            .and_then(|metadata| metadata.get("$ref"))
            .and_then(Value::as_str),
        Some("#/components/schemas/miaplatform.eu.v1.ObjectMetadata")
    );
}

#[test]
fn test_the_item_type_definition_fields_we_read_still_exist() {
    assert_properties(
        "miaplatform.eu.v1.ItemTypeDefinition",
        &["apiVersion", "kind", "metadata", "spec", "resourceVersion"],
    );

    let spec = schema("miaplatform.eu.v1.ItemTypeDefinition")
        .pointer("/properties/spec/oneOf/0/properties")
        .and_then(Value::as_object)
        .expect("the item-type-definition spec declares properties");

    for field in [
        "group",
        "names",
        "scope",
        "versions",
        "llmDescription",
        "history",
        "audit",
    ] {
        assert!(
            spec.contains_key(field),
            "this client reads `spec.{field}`, which the engine no longer declares"
        );
    }
}

#[test]
fn test_the_type_version_fields_we_read_still_exist() {
    let version = schema("miaplatform.eu.v1.ItemTypeDefinition")
        .pointer("/properties/spec/oneOf/0/properties/versions/items/properties")
        .and_then(Value::as_object)
        .expect("a type version declares properties");

    for field in ["name", "served", "deprecated", "schema", "selectableFields"] {
        assert!(
            version.contains_key(field),
            "this client reads `spec.versions[].{field}`, which the engine no longer declares"
        );
    }
}

/// The list envelope's `continue` is what pagination rests on, and its absence is what signals
/// the last page.
#[test]
fn test_the_list_envelope_still_carries_a_continue_token() {
    let metadata = schema("miaplatform.eu.v1.List_miaplatform.eu.v1.Item")
        .pointer("/properties/metadata/properties")
        .and_then(Value::as_object)
        .expect("the list envelope declares metadata properties");

    assert!(metadata.contains_key("continue"));
}

/// The error body is `{status, error, message}` and not RFC 7807, which is what the mapper
/// parses a message out of.
#[test]
fn test_the_error_body_is_still_the_engines_own_shape() {
    assert_properties(
        "miaplatform.eu.v1.StatusResponse",
        &["status", "error", "message"],
    );
}

/// Reads one declared parameter's schema.
fn declared_parameter_schema(spec: &OperationSpec, name: &str) -> &'static Value {
    operation(spec)
        .get("parameters")
        .and_then(Value::as_array)
        .expect("the operation declares parameters")
        .iter()
        .find(|parameter| parameter.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("`{}` no longer declares `{name}`", spec.path))
        .get("schema")
        .expect("a declared parameter has a schema")
}
