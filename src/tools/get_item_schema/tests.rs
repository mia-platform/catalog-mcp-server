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
    tools::get_item_schema::{
        GetItemSchema, GetItemSchemaInput, MAX_FIELDS, MAX_VERSION_BYTES,
        fields::{self, FieldSchemas},
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Remedy, ToolError,
    error::codes,
    testing::{MockEngine, mock_acl_context, mock_item_type_definition},
};
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

/// The type listing the `kind` lookup filters.
const TYPES_PATH: &str = "/mia-platform.eu/v1/item-type-definitions";

/// The per-call budget the fixtures run under.
const CALL_BUDGET: Duration = Duration::from_secs(25);

/// The recorded size of the realistic fixture's whole definition (DR-86). Regression detection,
/// not a limit.
const RECORDED_FULL_BYTES: usize = 1_447;

/// The recorded size of the same type asked for one field.
const RECORDED_FIELDS_BYTES: usize = 153;

/// How far those goldens may drift.
const SIZE_TOLERANCE_PERCENT: usize = 1;

/// A `Skill` type shaped like the engine's, carrying everything the engine sends: routing data to
/// be dropped, and fields this client's typed model does not declare, which must survive.
fn mock_skill_type() -> Value {
    let mut itd = mock_item_type_definition("Skill", "skills", "ai.mia-platform.eu");
    itd["metadata"] = json!({
        "name": "skills.ai.mia-platform.eu",
        "description": "A specialized guideline to accomplish a task",
        "uid": "550e8400-e29b-41d4-a716-446655440000",
        "urn": "urn:mia-platform-catalog:mia-platform.eu:v1:ItemTypeDefinition:skills.ai.mia-platform.eu",
        "family": "item-type-definitions",
        "creationTimestamp": "2026-09-17T10:30:45Z",
        "updateTimestamp": "2026-09-17T10:30:45Z"
    });
    itd["spec"]["names"]["displaySingular"] = json!("Skill");
    itd["spec"]["names"]["displayPlural"] = json!("Skills");
    itd["spec"]["llmDescription"] = json!("Use a skill when the task has a known procedure.");
    itd["spec"]["versions"][0]["schema"] = json!({ "openAPIV31Schema": mock_skill_schema() });
    itd["spec"]["versions"][0]["selectableFields"] = json!([{ "jsonPath": "spec.category" }]);
    itd["spec"]["versions"][0]["deprecationWarning"] = json!("Prefer v2 when it ships.");
    itd["spec"]["versions"][0]["x-extra"] = json!({ "kept": true });
    itd
}

/// A skill's item schema: one top-level `spec`, a nested object, an array of objects, a
/// recursive `$defs` reference and a field defined in two `oneOf` branches.
fn mock_skill_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "spec": {
            "type": "object",
            "required": ["category"],
            "properties": {
                "category": { "type": "string", "enum": ["how-to", "reference"] },
                "owner": { "type": "object", "properties": {
                    "team": { "type": "string", "description": "The owning team." }
                } },
                "steps": { "type": "array", "items": { "type": "object", "properties": {
                    "title": { "type": "string" }, "body": { "type": "string" }
                } } },
                "filter": { "$ref": "#/$defs/query" },
                "trigger": { "oneOf": [
                    { "properties": { "when": { "type": "string", "format": "date-time" } } },
                    { "properties": { "when": { "type": "string", "enum": ["always"] } } }
                ] }
            }
        } },
        "$defs": {
            "query": { "oneOf": [
                { "$ref": "#/$defs/leaf" },
                { "type": "object", "properties": {
                    "and": { "type": "array", "items": { "$ref": "#/$defs/query" } }
                } }
            ] },
            "leaf": { "type": "object", "properties": { "eq": { "type": "string" } } },
            "unused": { "type": "boolean" }
        }
    })
}

/// A JSON Schema `depth` levels deep and `width` properties wide at every level.
fn mock_schema(depth: usize, width: usize) -> Value {
    if depth == 0 {
        return json!({ "type": "string", "description": "A leaf field." });
    }

    let properties: serde_json::Map<String, Value> = (0..width)
        .map(|index| (format!("field{index}"), mock_schema(depth - 1, width)))
        .collect();

    json!({ "type": "object", "description": "A nested object.", "properties": properties })
}

/// A `List` envelope.
fn mock_page(items: Vec<Value>) -> Value {
    json!({ "apiVersion": "v1", "kind": "List", "metadata": {}, "items": items })
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

/// Serves `types` to the `kind` lookup for `kind`.
async fn mount_types(engine: &MockEngine, kind: &str, types: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param(
            "field",
            format!("spec.names.kind={kind}").as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(types)))
        .mount(engine.server())
        .await;
}

/// An input for `kind`, everything else absent.
fn mock_input(kind: &str) -> GetItemSchemaInput {
    GetItemSchemaInput {
        kind: kind.to_string(),
        group: None,
        version: None,
        fields: None,
    }
}

/// The same, asking for `paths`.
fn mock_fields_input(kind: &str, paths: &[&str]) -> GetItemSchemaInput {
    GetItemSchemaInput {
        fields: Some(paths.iter().map(|path| path.to_string()).collect()),
        ..mock_input(kind)
    }
}

/// Runs the tool against a lookup answering `types`.
async fn call_with(types: Vec<Value>, input: GetItemSchemaInput) -> Result<Value, ToolError> {
    let engine = MockEngine::start().await;
    let kind = input.kind.clone();
    mount_types(&engine, &kind, types).await;

    run(&mock_context_at(&engine.server().uri(), CALL_BUDGET), input).await
}

/// Runs the tool in `context`.
async fn run(context: &CallContext, input: GetItemSchemaInput) -> Result<Value, ToolError> {
    GetItemSchema
        .call(context, input)
        .await
        .map(|output| output.payload().clone())
}

// ---------------------------------------------------------------------------------------------
// DR-86 — the default is the whole definition, exactly as the engine sent it.
// ---------------------------------------------------------------------------------------------

/// The engine's `spec` comes back **untouched** — including fields this client's typed model does
/// not declare, which an edit built from a re-serialised copy would silently erase.
#[rstest]
#[tokio::test]
async fn test_the_default_is_the_whole_definition_untouched() {
    let payload = call_with(vec![mock_skill_type()], mock_input("Skill"))
        .await
        .expect("described");

    let keys: Vec<&str> = payload
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "kind", "group", "family", "version", "name", "metadata", "spec"
        ]
    );
    assert_eq!(payload["spec"], mock_skill_type()["spec"]);
    assert_eq!(
        payload["spec"]["versions"][0]["deprecationWarning"],
        json!("Prefer v2 when it ships.")
    );
    assert_eq!(payload["name"], json!("skills.ai.mia-platform.eu"));
    assert_eq!(payload["version"], json!("v1"));
}

/// Routing data is removed; what describes the type is kept.
#[rstest]
#[tokio::test]
async fn test_routing_data_is_removed() {
    let payload = call_with(vec![mock_skill_type()], mock_input("Skill"))
        .await
        .expect("described");

    assert_eq!(
        payload["metadata"],
        json!({ "description": "A specialized guideline to accomplish a task" })
    );
    for key in ["apiVersion", "resourceVersion"] {
        assert!(payload.get(key).is_none(), "`{key}` is not the type's");
    }
    assert_eq!(
        payload["kind"],
        json!("Skill"),
        "the type's kind, not `ItemTypeDefinition`"
    );
}

/// `metadata` made only of routing data is omitted rather than returned empty.
#[rstest]
#[tokio::test]
async fn test_metadata_with_nothing_left_is_omitted() {
    let mut itd = mock_skill_type();
    itd["metadata"]
        .as_object_mut()
        .expect("metadata")
        .remove("description");

    let payload = call_with(vec![itd], mock_input("Skill"))
        .await
        .expect("described");

    assert!(payload.get("metadata").is_none(), "{payload}");
}

/// A schema larger than the largest shipped type comes back byte for byte (§5, D34).
#[rstest]
#[tokio::test]
async fn test_a_large_schema_comes_back_whole() {
    let mut itd = mock_skill_type();
    let large = mock_schema(4, 6);
    itd["spec"]["versions"][0]["schema"] = json!({ "openAPIV31Schema": large.clone() });
    assert!(serde_json::to_string(&large).expect("serialises").len() > 20_000);

    let payload = call_with(vec![itd], mock_input("Skill"))
        .await
        .expect("described");

    assert_eq!(
        payload["spec"]["versions"][0]["schema"]["openAPIV31Schema"],
        large
    );
}

// ---------------------------------------------------------------------------------------------
// DR-86 — `fields`: the schema of just the fields being changed.
// ---------------------------------------------------------------------------------------------

/// Top-level and nested fields come back as their own schemas, in the order asked.
#[rstest]
#[tokio::test]
async fn test_fields_returns_each_fields_schema() {
    let payload = call_with(
        vec![mock_skill_type()],
        mock_fields_input("Skill", &["spec.owner.team", "spec.category"]),
    )
    .await
    .expect("described");

    assert_eq!(
        payload,
        json!({
            "kind": "Skill", "group": "ai.mia-platform.eu", "family": "skills", "version": "v1",
            "fields": {
                "spec.owner.team": { "type": "string", "description": "The owning team." },
                "spec.category": { "type": "string", "enum": ["how-to", "reference"] }
            }
        })
    );
}

/// A path crossing an array steps into its `items`.
#[rstest]
#[tokio::test]
async fn test_fields_steps_through_arrays() {
    let payload = call_with(
        vec![mock_skill_type()],
        mock_fields_input("Skill", &["spec.steps.title"]),
    )
    .await
    .expect("described");

    assert_eq!(
        payload["fields"]["spec.steps.title"],
        json!({ "type": "string" })
    );
}

/// A referenced definition comes back under `$defs` — transitively, once, and only if used —
/// so every `#/$defs/<name>` resolves against the answer, recursion included.
#[rstest]
#[tokio::test]
async fn test_fields_carries_the_definitions_it_references() {
    let payload = call_with(
        vec![mock_skill_type()],
        mock_fields_input("Skill", &["spec.filter"]),
    )
    .await
    .expect("described");

    assert_eq!(
        payload["fields"]["spec.filter"],
        json!({ "$ref": "#/$defs/query" })
    );
    let defs: Vec<&str> = payload["$defs"]
        .as_object()
        .expect("defs")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        defs,
        vec!["leaf", "query"],
        "the recursive definition terminates, the unused one is left out"
    );
}

/// A field defined in several branches comes back as all of them — which applies depends on the
/// item, and choosing would be a guess.
#[rstest]
#[tokio::test]
async fn test_a_field_in_several_branches_returns_them_all() {
    let payload = call_with(
        vec![mock_skill_type()],
        mock_fields_input("Skill", &["spec.trigger.when"]),
    )
    .await
    .expect("described");

    assert_eq!(
        payload["fields"]["spec.trigger.when"],
        json!({ "anyOf": [
            { "type": "string", "format": "date-time" },
            { "type": "string", "enum": ["always"] }
        ] })
    );
}

/// A path that names nothing is `invalid_input`, naming the fields that exist where it stopped.
#[rstest]
#[tokio::test]
async fn test_an_unknown_field_names_the_existing_ones() {
    let error = call_with(
        vec![mock_skill_type()],
        mock_fields_input("Skill", &["spec.owner.email"]),
    )
    .await
    .expect_err("no such field");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["validFields"].clone()),
        Some(json!(["spec.owner.team"]))
    );
}

/// `fields` reads the requested version's schema, not the served one's.
#[rstest]
#[tokio::test]
async fn test_fields_reads_the_requested_version() {
    let payload = call_with(
        vec![mock_two_versions()],
        GetItemSchemaInput {
            version: Some("v2".to_string()),
            ..mock_fields_input("Skill", &["spec.category"])
        },
    )
    .await
    .expect("described");

    assert_eq!(payload["version"], json!("v2"));
    assert_eq!(
        payload["fields"]["spec.category"],
        json!({ "type": "integer" })
    );
}

/// `fields` bounds: at least one path, at most `MAX_FIELDS`, each a dotted name.
#[rstest]
#[case::empty(vec![])]
#[case::too_many((0..=MAX_FIELDS).map(|index| format!("spec.f{index}")).collect())]
#[case::empty_segment(vec!["spec..category".to_string()])]
#[tokio::test]
async fn test_fields_out_of_bounds_is_refused(#[case] paths: Vec<String>) {
    let engine = MockEngine::start().await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        GetItemSchemaInput {
            fields: Some(paths),
            ..mock_input("Skill")
        },
    )
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
        Some(json!("fields"))
    );
}

/// Extracts `paths` from `root` directly.
fn extract(root: &Value, paths: &[&str]) -> Result<FieldSchemas, ToolError> {
    let paths: Vec<String> = paths.iter().map(|path| path.to_string()).collect();
    fields::extract(root, &paths)
}

/// A `$ref` sitting beside other keywords is followed **and** the siblings are kept (2020-12).
#[rstest]
fn test_a_reference_beside_properties_keeps_both() {
    let root = json!({
        "$ref": "#/$defs/base",
        "properties": { "own": { "type": "string" } },
        "$defs": { "base": { "properties": { "inherited": { "type": "integer" } } } }
    });

    let found = extract(&root, &["own", "inherited"]).expect("both are fields");

    assert_eq!(found.fields["own"], json!({ "type": "string" }));
    assert_eq!(found.fields["inherited"], json!({ "type": "integer" }));
}

/// A definition that is only a reference to another is followed to the end of the chain.
#[rstest]
fn test_a_chain_of_references_is_followed() {
    let root = json!({
        "$ref": "#/$defs/alias",
        "$defs": {
            "alias": { "$ref": "#/$defs/target" },
            "target": { "properties": { "name": { "type": "string" } } }
        }
    });

    let found = extract(&root, &["name"]).expect("reached through the alias");

    assert_eq!(found.fields["name"], json!({ "type": "string" }));
}

/// A reference cycle that consumes no path segment stops instead of recursing without end — in the
/// walk and in the error listing the fields that exist.
#[rstest]
fn test_a_reference_cycle_terminates() {
    let root = json!({
        "$ref": "#/$defs/loop",
        "$defs": { "loop": { "anyOf": [
            { "$ref": "#/$defs/loop" },
            { "properties": { "name": { "type": "string" } } }
        ] } }
    });

    let found = extract(&root, &["name"]).expect("found once");
    let error = extract(&root, &["missing"]).err().expect("not a field");

    assert_eq!(found.fields["name"], json!({ "type": "string" }));
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["validFields"].clone()),
        Some(json!(["name"]))
    );
}

/// A discriminated union — the property declared on the object and narrowed per branch, as
/// `ScorecardEvaluation`'s `status` is — keeps both halves: the declaration always applies, one
/// narrowing depending on the item. Never the looser union of the three.
#[rstest]
fn test_a_discriminated_union_keeps_the_base_and_the_branches_apart() {
    let root = json!({
        "properties": { "status": { "type": "string", "enum": ["pending", "error"] } },
        "oneOf": [
            { "properties": { "status": { "enum": ["pending"] } } },
            { "properties": { "status": { "enum": ["error"] } } }
        ]
    });

    let found = extract(&root, &["status"]).expect("a field");

    assert_eq!(
        found.fields["status"],
        json!({ "allOf": [
            { "type": "string", "enum": ["pending", "error"] },
            { "anyOf": [{ "enum": ["pending"] }, { "enum": ["error"] }] }
        ] })
    );
}

/// Every `allOf` branch defining a field applies, so they come back together under `allOf`.
#[rstest]
fn test_all_of_branches_all_apply() {
    let root = json!({ "allOf": [
        { "properties": { "name": { "type": "string" } } },
        { "properties": { "name": { "maxLength": 63 } } }
    ] });

    let found = extract(&root, &["name"]).expect("a field");

    assert_eq!(
        found.fields["name"],
        json!({ "allOf": [{ "type": "string" }, { "maxLength": 63 }] })
    );
}

/// The walk continues through a composed answer: the next segment is looked up in every part.
#[rstest]
fn test_a_path_continues_through_a_composed_field() {
    let root = json!({ "properties": { "spec": { "oneOf": [
        { "properties": { "port": { "properties": { "number": { "type": "integer" } } } } },
        { "properties": { "port": { "properties": { "number": { "type": "string" } } } } }
    ] } } });

    let found = extract(&root, &["spec.port.number"]).expect("a field");

    assert_eq!(
        found.fields["spec.port.number"],
        json!({ "anyOf": [{ "type": "integer" }, { "type": "string" }] })
    );
}

/// The same schema reached by two routes is one answer, not an `anyOf` of itself.
#[rstest]
fn test_one_schema_reached_twice_is_returned_once() {
    let root = json!({
        "allOf": [{ "$ref": "#/$defs/shared" }],
        "anyOf": [{ "$ref": "#/$defs/shared" }],
        "$defs": { "shared": { "properties": { "name": { "type": "string" } } } }
    });

    let found = extract(&root, &["name"]).expect("a field");

    assert_eq!(found.fields["name"], json!({ "type": "string" }));
}

// ---------------------------------------------------------------------------------------------
// T6-D8 — versions.
// ---------------------------------------------------------------------------------------------

/// A type with two served versions, the older deprecated, whose schemas differ.
fn mock_two_versions() -> Value {
    let mut itd = mock_skill_type();
    let mut v2 = itd["spec"]["versions"][0].clone();
    v2["name"] = json!("v2");
    v2["schema"] = json!({ "openAPIV31Schema": { "type": "object", "properties": { "spec": {
        "type": "object", "properties": { "category": { "type": "integer" } }
    } } } });
    itd["spec"]["versions"][0]["deprecated"] = json!(true);
    itd["spec"]["versions"]
        .as_array_mut()
        .expect("versions")
        .push(v2);
    itd
}

/// Absent `version`: the core's rule picks — a non-deprecated one wins.
#[rstest]
#[tokio::test]
async fn test_the_served_version_is_selected_by_the_core_rule() {
    let payload = call_with(vec![mock_two_versions()], mock_input("Skill"))
        .await
        .expect("described");

    assert_eq!(payload["version"], json!("v2"));
    assert_eq!(
        payload["spec"]["versions"].as_array().map(Vec::len),
        Some(2),
        "every version is there"
    );
}

/// An unknown or unserved version is an error naming the served ones — never substituted.
#[rstest]
#[case::unknown("v9")]
#[case::unserved("v3")]
#[tokio::test]
async fn test_a_version_that_is_not_served_is_refused_naming_the_served(#[case] version: &str) {
    let mut itd = mock_two_versions();
    let mut v3 = itd["spec"]["versions"][0].clone();
    v3["name"] = json!("v3");
    v3["served"] = json!(false);
    itd["spec"]["versions"]
        .as_array_mut()
        .expect("versions")
        .push(v3);

    let error = call_with(
        vec![itd],
        GetItemSchemaInput {
            version: Some(version.to_string()),
            ..mock_input("Skill")
        },
    )
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
            .map(|details| details["servedVersions"].clone()),
        Some(json!(["v1", "v2"]))
    );
}

/// A type with no served version is `unaddressable_type`.
#[rstest]
#[tokio::test]
async fn test_a_type_with_nothing_served_is_unaddressable() {
    let mut itd = mock_skill_type();
    itd["spec"]["versions"][0]["served"] = json!(false);

    let error = call_with(vec![itd], mock_input("Skill"))
        .await
        .expect_err("unaddressable");

    assert_eq!(
        (error.code, error.remedy),
        (codes::UNADDRESSABLE_TYPE, Remedy::Escalate)
    );
}

// ---------------------------------------------------------------------------------------------
// DR-80 and §6 — which type, and the rest of the error table.
// ---------------------------------------------------------------------------------------------

/// A kind shared by two groups is answered with the candidates; neither is picked.
#[rstest]
#[tokio::test]
async fn test_a_shared_kind_returns_the_candidates() {
    let mut other = mock_skill_type();
    other["spec"]["group"] = json!("example.com");
    other["metadata"]["name"] = json!("skills.example.com");

    let error = call_with(vec![mock_skill_type(), other], mock_input("Skill"))
        .await
        .expect_err("never guessed");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!([
            { "kind": "Skill", "group": "ai.mia-platform.eu", "family": "skills" },
            { "kind": "Skill", "group": "example.com", "family": "skills" }
        ]))
    );
}

/// With `group` the lookup is exact — one request, asking for two rows.
#[rstest]
#[tokio::test]
async fn test_a_group_makes_the_lookup_exact() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.group=ai.mia-platform.eu"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_skill_type()])))
        .mount(engine.server())
        .await;

    let payload = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        GetItemSchemaInput {
            group: Some("ai.mia-platform.eu".to_string()),
            ..mock_input("Skill")
        },
    )
    .await
    .expect("the pair names one type");

    assert_eq!(payload["group"], json!("ai.mia-platform.eu"));
    let requests = engine
        .server()
        .received_requests()
        .await
        .unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .url
            .query()
            .is_some_and(|query| query.contains("limit=2"))
    );
}

/// Two rows for one `(group, kind)` break the engine's own constraint: `server_defect` (T6-D2).
#[rstest]
#[tokio::test]
async fn test_two_types_for_one_group_and_kind_are_a_server_defect() {
    let engine = MockEngine::start().await;
    let mut duplicate = mock_skill_type();
    duplicate["metadata"]["name"] = json!("skills.ai.mia-platform.eu-again");
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param("field", "spec.group=ai.mia-platform.eu"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_skill_type(), duplicate])),
        )
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        GetItemSchemaInput {
            group: Some("ai.mia-platform.eu".to_string()),
            ..mock_input("Skill")
        },
    )
    .await
    .expect_err("an impossible pair is never guessed");

    assert_eq!(
        (error.code, error.remedy),
        (codes::SERVER_DEFECT, Remedy::Escalate)
    );
}

/// An unknown `kind` returns candidates, not a bare `404`.
#[rstest]
#[tokio::test]
async fn test_an_unknown_kind_returns_candidates() {
    let engine = MockEngine::start().await;
    mount_types(&engine, "Skil", vec![]).await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .and(query_param_is_missing("field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_page(vec![mock_skill_type()])))
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        mock_input("Skil"),
    )
    .await
    .expect_err("an unknown kind fails");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert_eq!(
        error
            .details
            .as_deref()
            .map(|details| details["candidates"].clone()),
        Some(json!(["Skill"]))
    );
    assert!(
        error
            .next_step
            .as_deref()
            .is_some_and(|step| step.contains("list_catalog_types"))
    );
}

/// Engine unavailability is `catalog_unavailable`.
#[rstest]
#[case::server_error(500)]
#[case::unavailable(503)]
#[tokio::test]
async fn test_an_engine_failure_is_catalog_unavailable(#[case] status: u16) {
    let engine = MockEngine::start().await;
    engine.get_error(TYPES_PATH, status, "boom").await;

    let error = run(
        &mock_context_at(&engine.server().uri(), CALL_BUDGET),
        mock_input("Skill"),
    )
    .await
    .expect_err("fails");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CATALOG_UNAVAILABLE, Remedy::Retry)
    );
}

/// The call's own budget running out is `deadline_exceeded`.
#[rstest]
#[tokio::test]
async fn test_a_spent_deadline_is_deadline_exceeded() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(TYPES_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(mock_page(vec![mock_skill_type()]))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(engine.server())
        .await;

    let error = run(
        &mock_context_at(&engine.server().uri(), Duration::from_millis(100)),
        mock_input("Skill"),
    )
    .await
    .expect_err("the deadline runs out first");

    assert_eq!(
        (error.code, error.remedy),
        (codes::DEADLINE_EXCEEDED, Remedy::Retry)
    );
}

/// Boundary bounds are `invalid_input` naming the parameter, before the engine is asked.
#[rstest]
#[case::empty_kind(mock_input(""), "kind")]
#[case::bad_kind(mock_input("not a kind"), "kind")]
#[case::long_kind(mock_input(&"K".repeat(129)), "kind")]
#[case::empty_version(GetItemSchemaInput { version: Some(String::new()), ..mock_input("Skill") }, "version")]
#[case::long_version(
    GetItemSchemaInput { version: Some("v".repeat(MAX_VERSION_BYTES + 1)), ..mock_input("Skill") },
    "version"
)]
#[case::bad_group(GetItemSchemaInput { group: Some("Not A Group".to_string()), ..mock_input("Skill") }, "group")]
#[tokio::test]
async fn test_an_input_over_its_bound_is_refused(
    #[case] input: GetItemSchemaInput,
    #[case] field: &str,
) {
    let engine = MockEngine::start().await;

    let error = run(&mock_context_at(&engine.server().uri(), CALL_BUDGET), input)
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
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "refused before the engine"
    );
}

// ---------------------------------------------------------------------------------------------
// The byte goldens.
// ---------------------------------------------------------------------------------------------

/// The realistic type's two answers serialise to recorded sizes — a regression in either shape is
/// a diff — and the `fields` answer is a small fraction of the whole.
#[rstest]
#[tokio::test]
async fn test_both_answers_serialise_to_their_recorded_sizes() {
    let full = call_with(vec![mock_skill_type()], mock_input("Skill"))
        .await
        .expect("full");
    let fields = call_with(
        vec![mock_skill_type()],
        mock_fields_input("Skill", &["spec.category"]),
    )
    .await
    .expect("fields");
    let (full, fields) = (
        serde_json::to_string(&full).expect("serialises").len(),
        serde_json::to_string(&fields).expect("serialises").len(),
    );

    for (actual, recorded, which) in [
        (full, RECORDED_FULL_BYTES, "the whole definition"),
        (fields, RECORDED_FIELDS_BYTES, "one field"),
    ] {
        let tolerance = recorded * SIZE_TOLERANCE_PERCENT / 100;
        assert!(
            actual.abs_diff(recorded) <= tolerance,
            "{which} serialises to {actual} B, recorded {recorded} B (±{SIZE_TOLERANCE_PERCENT} %)."
        );
    }
    assert!(
        fields * 5 < full,
        "one field ({fields} B) is not a fraction of the whole ({full} B)"
    );
}
