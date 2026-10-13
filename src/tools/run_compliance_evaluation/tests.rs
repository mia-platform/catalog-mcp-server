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
// Against a mock engine, with synthetic compliance items only — fictional names; the repository is
// public. The poll runs at millisecond intervals here; the schedule itself is tested on its own.

use crate::{
    registry::contract::{CallContext, Tool},
    tools::run_compliance_evaluation::{
        INTERVALS, RunComplianceEvaluation, RunComplianceEvaluationInput, interval, progress_point,
    },
};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Projection, Remedy, ToolError,
    error::codes,
    testing::{
        MockEngine, mock_acl_context, mock_error_body, mock_item_type_definition,
        mock_list_envelope,
    },
};
use configuration::tools::ToolDefaults;
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{header, method, path, path_regex},
};

const COMPLIANCE: &str = "/compliance.mia-platform.eu/v1/items";
const RULE: &str = "my-rule";
const RUN: &str = "run-my-rule-1760000000";
const SCORECARD: &str = "my-card";
const EVALUATION: &str = "my-card-evaluation-1760000000";
const CAMPAIGN: &str = "my-campaign";
const VIEW: &str = "my-view";

/// Poll intervals for tests: milliseconds, not seconds.
static FAST: [Duration; 1] = [Duration::from_millis(5)];

/// A call budget comfortably above every wait used here.
const BUDGET: Duration = Duration::from_secs(10);

fn tool() -> RunComplianceEvaluation {
    RunComplianceEvaluation::with_intervals(&ToolDefaults::default(), &FAST)
}

fn mock_context_with(engine: &MockEngine, cancellation: CancellationToken) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
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
    .bind(identity.clone(), Deadline::starting_now(BUDGET));

    CallContext::new(
        client,
        Deadline::starting_now(BUDGET),
        cancellation,
        None,
        identity.tenant_key(),
    )
}

fn mock_context(engine: &MockEngine) -> CallContext {
    mock_context_with(engine, CancellationToken::new())
}

/// Calls the tool with `arguments`; the answer as the model reads it.
async fn call(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    call_with(engine, &mock_context(engine), arguments).await
}

async fn call_with(
    _engine: &MockEngine,
    context: &CallContext,
    arguments: Value,
) -> Result<Value, ToolError> {
    let input: RunComplianceEvaluationInput =
        serde_json::from_value(arguments).expect("the arguments deserialise");

    tool()
        .call(context, input)
        .await
        .map(|output| output.render(None))
}

/// A compliance item as the engine returns it.
fn compliance_item(kind: &str, name: &str, spec: Value, version: &str) -> Value {
    json!({
        "apiVersion": "compliance.mia-platform.eu/v1",
        "kind": kind,
        "metadata": {
            "name": name,
            "urn": format!("urn:mia-platform-catalog:compliance.mia-platform.eu:v1:{kind}:{name}"),
            "creationTimestamp": "2026-01-01T00:00:00Z"
        },
        "spec": spec,
        "resourceVersion": version
    })
}

/// A View, as the engine returns it.
fn view_item() -> Value {
    json!({
        "apiVersion": "mia-platform.eu/v1",
        "kind": "View",
        "metadata": { "name": VIEW, "urn": format!("urn:mia-platform-catalog:mia-platform.eu:v1:View:{VIEW}") },
        "spec": {}
    })
}

/// An item URN of the fictional `example.com` group.
fn urn(kind: &str, name: &str) -> String {
    format!("urn:mia-platform-catalog:example.com:v1:{kind}:{name}")
}

/// Mounts a full read of `family/name` answering `body`, at most `times` times when given.
async fn mount_full(
    engine: &MockEngine,
    family: &str,
    name: &str,
    body: Value,
    times: Option<u64>,
) {
    let mock = Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/{family}/{name}")))
        .and(header("accept", Projection::Full.accept()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body));
    let mock = match times {
        Some(times) => mock.up_to_n_times(times),
        None => mock,
    };
    mock.mount(engine.server()).await;
}

/// Mounts a metadata read of `family/name` answering `version`.
async fn mount_version(
    engine: &MockEngine,
    family: &str,
    name: &str,
    version: &str,
    times: Option<u64>,
) {
    let mock = Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/{family}/{name}")))
        .and(header("accept", Projection::PartialObjectMetadata.accept()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "compliance.mia-platform.eu/v1", "kind": "X",
            "metadata": { "name": name }, "resourceVersion": version
        })));
    let mock = match times {
        Some(times) => mock.up_to_n_times(times),
        None => mock,
    };
    mock.mount(engine.server()).await;
}

/// Mounts the evaluate POST of `family/name`, answering `status` with `body`.
async fn mount_post(engine: &MockEngine, family: &str, name: &str, status: u16, body: Value) {
    Mock::given(method("POST"))
        .and(path(format!("{COMPLIANCE}/{family}/{name}/evaluate")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(engine.server())
        .await;
}

/// Mounts an empty global listing, which near-match searches read.
async fn mount_no_candidates(engine: &MockEngine) {
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;
}

/// The rule, with optional target types, and its View.
async fn mount_rule(engine: &MockEngine, target_item_types: Option<Value>) {
    let mut spec = json!({ "body": { "type": "cel", "content": "true" } });
    if let Some(types) = target_item_types {
        spec["targetItemTypes"] = types;
    }
    mount_full(
        engine,
        "rules",
        RULE,
        compliance_item("Rule", RULE, spec, "1"),
        None,
    )
    .await;

    Mock::given(method("GET"))
        .and(path(format!("/mia-platform.eu/v1/items/views/{VIEW}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(view_item()))
        .mount(engine.server())
        .await;
}

/// A RuleRun with `results`.
fn rule_run(results: Value, version: &str) -> Value {
    let mut spec = json!({
        "ruleRef": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:my-rule",
        "runStartTime": "2026-01-01T00:00:00Z",
        "scope": { "type": "view-ref", "content": "urn" },
        "results": results
    });
    if results["status"] != json!("pending") {
        spec["runEndTime"] = json!("2026-01-01T00:00:04Z");
    }
    compliance_item("RuleRun", RUN, spec, version)
}

/// Mounts a rule evaluation that starts `RUN`.
async fn mount_rule_post(engine: &MockEngine) {
    mount_post(
        engine,
        "rules",
        RULE,
        201,
        json!({ "urn": format!("urn:mia-platform-catalog:compliance.mia-platform.eu:v1:RuleRun:{RUN}") }),
    )
    .await;
}

/// How many requests of `verb` reached `path_suffix`, with `accept` when given.
async fn count(engine: &MockEngine, verb: &str, path_suffix: &str, accept: Option<&str>) -> usize {
    engine
        .server()
        .received_requests()
        .await
        .expect("the mock records its requests")
        .iter()
        .filter(|request| request.method.as_str() == verb)
        .filter(|request| request.url.path().ends_with(path_suffix))
        .filter(|request| {
            accept.is_none_or(|accept| {
                request
                    .headers
                    .get("accept")
                    .and_then(|value| value.to_str().ok())
                    == Some(accept)
            })
        })
        .count()
}

fn rule_args(extra: Value) -> Value {
    let mut arguments =
        json!({ "target": "rule", "name": RULE, "scope": { "view": VIEW }, "wait": 2 });
    arguments
        .as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    arguments
}

// ---------------------------------------------------------------------------------------------
// validation
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::name_pattern(json!({ "target": "scorecard", "name": "Not_A_Name" }), "name")]
#[case::name_too_long(json!({ "target": "scorecard", "name": "a".repeat(254) }), "name")]
#[case::scope_missing_for_rule(json!({ "target": "rule", "name": RULE }), "scope")]
#[case::scope_for_scorecard(json!({ "target": "scorecard", "name": SCORECARD, "scope": { "view": VIEW } }), "scope")]
#[case::scope_for_campaign(json!({ "target": "campaign", "name": CAMPAIGN, "scope": { "kind": "Service" } }), "scope")]
#[case::view_with_filters(json!({ "target": "rule", "name": RULE, "scope": { "view": VIEW, "kind": "Service" } }), "scope")]
#[case::empty_scope(json!({ "target": "rule", "name": RULE, "scope": {} }), "scope")]
#[case::view_urn_of_other_kind(json!({ "target": "rule", "name": RULE, "scope": { "view": "urn:mia-platform-catalog:mia-platform.eu:v1:Rule:x" } }), "scope.view")]
#[case::group_without_kind(json!({ "target": "rule", "name": RULE, "scope": { "group": "example.com" } }), "group")]
#[tokio::test]
async fn test_validation(#[case] arguments: Value, #[case] field: &str) {
    let engine = MockEngine::start().await;

    let error = call(&engine, arguments).await.expect_err("refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    let details = error.details.expect("details name the field");
    assert_eq!(details["field"], json!(field));
    assert!(
        engine
            .server()
            .received_requests()
            .await
            .expect("recorded")
            .is_empty(),
        "nothing reached the engine"
    );
}

/// A mistyped field is refused, never ignored: inside `scope`, it would widen the evaluation.
#[rstest]
#[case::unknown_field_in_scope(json!({ "target": "rule", "name": RULE, "scope": { "kind": "Service", "label": { "a": "b" } } }))]
#[case::unknown_field_top_level(json!({ "target": "rule", "name": RULE, "scope": { "view": VIEW }, "waits": 1 }))]
#[case::unknown_target(json!({ "target": "policy", "name": RULE }))]
fn test_unknown_fields_are_refused(#[case] arguments: Value) {
    assert!(serde_json::from_value::<RunComplianceEvaluationInput>(arguments).is_err());
}

// ---------------------------------------------------------------------------------------------
// a rule, end to end
// ---------------------------------------------------------------------------------------------

/// Pending first, then complete: one round trip, counts and failures, out-of-target items apart.
#[rstest]
#[tokio::test]
async fn test_a_rule_completes_with_out_of_target_items_apart() {
    let engine = MockEngine::start().await;
    mount_rule(
        &engine,
        Some(json!({ "mode": "include", "itemTypeRefs": ["urn:mia-platform-catalog:example.com:v1:Service"] })),
    )
    .await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "pending", "data": null }), "1"),
        Some(1),
    )
    .await;
    mount_version(&engine, "rule-runs", RUN, "1", Some(1)).await;
    mount_version(&engine, "rule-runs", RUN, "2", None).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(
            json!({ "status": "complete", "data": [
                { "itemRef": urn("Service", "svc-a"), "result": true },
                { "itemRef": urn("Service", "svc-b"), "result": false },
                { "itemRef": urn("Project", "prj-a"), "result": false }
            ] }),
            "2",
        ),
        None,
    )
    .await;

    let answer = call(&engine, rule_args(json!({}))).await.expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(answer["total"], json!(3));
    assert_eq!(answer["passed"], json!(1));
    assert_eq!(answer["failed"], json!(1));
    assert_eq!(answer["outOfTarget"], json!(1));
    assert_eq!(
        answer["failures"],
        json!([{ "name": "svc-b", "kind": "Service", "group": "example.com" }])
    );
    assert_eq!(answer["startedAt"], json!("2026-01-01T00:00:00Z"));
    assert_eq!(answer["endedAt"], json!("2026-01-01T00:00:04Z"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "RuleRun", "name": RUN })
    );
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 1);
}

/// A metadata poll that sees the same version reads nothing more; a change costs one full read.
#[rstest]
#[tokio::test]
async fn test_poll_loop_metadata_poll_full_read_only_on_change() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "pending", "data": null }), "1"),
        Some(1),
    )
    .await;
    mount_version(&engine, "rule-runs", RUN, "1", Some(5)).await;
    mount_version(&engine, "rule-runs", RUN, "2", None).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "complete", "data": [] }), "2"),
        None,
    )
    .await;

    call(&engine, rule_args(json!({}))).await.expect("a result");

    let full = count(
        &engine,
        "GET",
        &format!("/rule-runs/{RUN}"),
        Some(Projection::Full.accept()),
    )
    .await;
    let metadata = count(
        &engine,
        "GET",
        &format!("/rule-runs/{RUN}"),
        Some(Projection::PartialObjectMetadata.accept()),
    )
    .await;
    assert_eq!(
        full, 2,
        "one full read at first, one after the version changed"
    );
    assert_eq!(metadata, 6, "five unchanged polls, then the changed one");
}

#[rstest]
#[tokio::test]
async fn test_zero_scope_is_not_a_pass() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "complete", "data": [] }), "1"),
        None,
    )
    .await;

    let answer = call(&engine, rule_args(json!({}))).await.expect("a result");

    assert_eq!(answer["total"], json!(0));
    let warnings = answer["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("proves nothing"))),
        "{warnings:?}"
    );
}

#[rstest]
#[tokio::test]
async fn test_an_error_run_is_a_result_with_no_counts() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(
            json!({ "status": "error", "data": { "error": "Failed to fetch scope items" } }),
            "1",
        ),
        None,
    )
    .await;

    let answer = call(&engine, rule_args(json!({})))
        .await
        .expect("an error run is a result");

    assert_eq!(answer["status"], json!("error"));
    assert_eq!(answer["error"], json!("Failed to fetch scope items"));
    for field in ["total", "passed", "failed", "failures", "failedByKind"] {
        assert!(
            answer.get(field).is_none(),
            "`{field}` must not be readable as a pass"
        );
    }
}

#[rstest]
#[tokio::test]
async fn test_wait_zero_one_post_no_poll() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;

    let answer = call(&engine, rule_args(json!({ "wait": 0 })))
        .await
        .expect("a hand-back");

    assert_eq!(answer["status"], json!("running"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "RuleRun", "name": RUN })
    );
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 1);
    assert_eq!(
        count(&engine, "GET", &format!("/rule-runs/{RUN}"), None).await,
        0
    );
}

/// A run still pending when the wait ends is handed back with its start time and the follow-up.
#[rstest]
#[tokio::test]
async fn test_budget_exhausted_hands_back() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "pending", "data": null }), "1"),
        None,
    )
    .await;
    mount_version(&engine, "rule-runs", RUN, "1", None).await;

    let answer = call(&engine, rule_args(json!({ "wait": 1 })))
        .await
        .expect("a hand-back");

    assert_eq!(answer["status"], json!("running"));
    assert_eq!(answer["startedAt"], json!("2026-01-01T00:00:00Z"));
    assert!(answer.get("total").is_none());
    let warning = answer["warnings"][0].as_str().expect("a warning");
    assert!(
        warning.contains("get_compliance_results") && warning.contains(RUN),
        "{warning}"
    );
}

/// The filters become a plain-JSON `raw-query` with the resolved type stated in it.
#[rstest]
#[tokio::test]
async fn test_scope_encoding_filters_are_plain_json_with_the_type_stated() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
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
    mount_rule_post(&engine).await;

    call(
        &engine,
        json!({ "target": "rule", "name": RULE, "wait": 0,
                "scope": { "kind": "Service", "labels": { "tier": "gold" } } }),
    )
    .await
    .expect("a hand-back");

    let post = engine
        .server()
        .received_requests()
        .await
        .expect("recorded")
        .into_iter()
        .find(|request| request.method.as_str() == "POST")
        .expect("one POST");
    let body: Value = serde_json::from_slice(&post.body).expect("a JSON body");
    assert_eq!(body["scope"]["type"], json!("raw-query"));
    assert_eq!(
        body["scope"]["content"],
        json!({ "and": [
            { "apiVersion": { "eq": "stable.example.com/v1" } },
            { "kind": { "eq": "Service" } },
            { "and": [ { "metadata.labels.tier": { "eq": "gold" } } ] }
        ] })
    );
}

// ---------------------------------------------------------------------------------------------
// a scorecard and a campaign
// ---------------------------------------------------------------------------------------------

async fn mount_scorecard(engine: &MockEngine, pending: Vec<Value>) {
    mount_full(
        engine,
        "scorecards",
        SCORECARD,
        compliance_item(
            "Scorecard",
            SCORECARD,
            json!({
                "levels": [ { "name": "gold" }, { "name": "silver" } ],
                "rules": [
                    { "type": "inline", "content": { "name": "rule-silver" }, "level": "silver" },
                    { "type": "rule-ref", "content": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:rule-gold", "level": "gold" }
                ],
                "scope": {}
            }),
            "1",
        ),
        None,
    )
    .await;

    // The pre-flight's pending check, then the locator's newest evaluation.
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/scorecard-evaluations")))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(pending, None)))
        .up_to_n_times(1)
        .mount(engine.server())
        .await;
}

fn evaluation_item(evaluation: Value, version: &str) -> Value {
    compliance_item(
        "ScorecardEvaluation",
        EVALUATION,
        json!({ "scorecardRef": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Scorecard:my-card",
                "evaluation": evaluation }),
        version,
    )
}

#[rstest]
#[tokio::test]
async fn test_a_scorecard_completes_with_levels() {
    let engine = MockEngine::start().await;
    mount_scorecard(&engine, vec![]).await;
    mount_post(
        &engine,
        "scorecards",
        SCORECARD,
        202,
        json!({ "urn": "urn:x" }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/scorecard-evaluations")))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(
            vec![evaluation_item(
                json!({ "status": "pending", "data": { "startTime": "2026-01-01T00:00:00Z" } }),
                "1",
            )],
            None,
        )))
        .mount(engine.server())
        .await;
    mount_full(
        &engine,
        "scorecard-evaluations",
        EVALUATION,
        evaluation_item(
            json!({ "status": "complete", "data": {
                "startTime": "2026-01-01T00:00:00Z", "endTime": "2026-01-01T00:00:03Z",
                "results": [
                    { "itemRef": urn("Project", "gold-one"), "rules": { "rule-silver": true, "rule-gold": true } },
                    { "itemRef": urn("Project", "silver-one"), "rules": { "rule-silver": true, "rule-gold": false } }
                ] } }),
            "2",
        ),
        None,
    )
    .await;

    let answer = call(
        &engine,
        json!({ "target": "scorecard", "name": SCORECARD, "wait": 2 }),
    )
    .await
    .expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(answer["minimumLevel"], json!("silver"));
    assert_eq!(answer["byLevel"], json!({ "gold": 1, "silver": 1 }));
    assert_eq!(answer["failures"][0]["failedRules"], json!(["rule-gold"]));
    assert_eq!(answer["failures"][0]["level"], json!("silver"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "ScorecardEvaluation", "name": EVALUATION })
    );
}

/// A campaign still showing its previous run's `complete` is not taken for the new one.
#[rstest]
#[tokio::test]
async fn test_locator_campaign_stale_complete_ignored() {
    let engine = MockEngine::start().await;
    let campaign = |last: Value, results: Value, version: &str| {
        compliance_item(
            "Campaign",
            CAMPAIGN,
            json!({ "startTime": "2020-01-01T00:00:00Z", "endTime": "2999-01-01T00:00:00Z",
                    "rules": [], "scope": {}, "lastEvaluationResult": last, "results": results }),
            version,
        )
    };
    let stale = json!({ "status": "complete", "data": { "startTime": "2020-06-01T00:00:00Z", "endTime": "2020-06-01T00:00:05Z" } });
    let stale_results = json!([ { "itemRef": urn("Project", "old"), "rules": { "goal": false } } ]);

    // Pre-flight, then the first poll: still the previous run.
    mount_full(
        &engine,
        "campaigns",
        CAMPAIGN,
        campaign(stale.clone(), stale_results.clone(), "1"),
        Some(2),
    )
    .await;
    mount_version(&engine, "campaigns", CAMPAIGN, "1", Some(1)).await;
    mount_version(&engine, "campaigns", CAMPAIGN, "2", None).await;
    // Later than any POST: the guard accepts it; the stale run above it, it never does.
    let now = "2998-01-01T00:00:00Z";
    mount_full(
        &engine,
        "campaigns",
        CAMPAIGN,
        campaign(
            json!({ "status": "complete", "data": { "startTime": now, "endTime": now } }),
            json!([ { "itemRef": urn("Project", "new"), "rules": { "goal": true } } ]),
            "2",
        ),
        None,
    )
    .await;
    mount_post(
        &engine,
        "campaigns",
        CAMPAIGN,
        202,
        json!({ "urn": "urn:x" }),
    )
    .await;

    let answer = call(
        &engine,
        json!({ "target": "campaign", "name": CAMPAIGN, "wait": 2 }),
    )
    .await
    .expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(
        answer["passed"],
        json!(1),
        "the new run's results, not the stale ones"
    );
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "Campaign", "name": CAMPAIGN })
    );
}

// ---------------------------------------------------------------------------------------------
// preflight
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_preflight_rule_unknown() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rules/{RULE}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(mock_error_body(404, "not found")))
        .mount(engine.server())
        .await;
    mount_no_candidates(&engine).await;

    let error = call(&engine, rule_args(json!({})))
        .await
        .expect_err("unknown");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 0);
}

#[rstest]
#[tokio::test]
async fn test_preflight_view_unknown() {
    let engine = MockEngine::start().await;
    mount_full(
        &engine,
        "rules",
        RULE,
        compliance_item("Rule", RULE, json!({ "body": {} }), "1"),
        None,
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/mia-platform.eu/v1/items/views/{VIEW}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(mock_error_body(404, "not found")))
        .mount(engine.server())
        .await;
    mount_no_candidates(&engine).await;

    let error = call(&engine, rule_args(json!({})))
        .await
        .expect_err("unknown view");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 0);
}

#[rstest]
#[tokio::test]
async fn test_preflight_scorecard_pending() {
    let engine = MockEngine::start().await;
    mount_scorecard(
        &engine,
        vec![evaluation_item(
            json!({ "status": "pending", "data": { "startTime": "2026-01-01T00:00:00Z" } }),
            "1",
        )],
    )
    .await;

    let error = call(&engine, json!({ "target": "scorecard", "name": SCORECARD }))
        .await
        .expect_err("already pending");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, Remedy::RetryLater);
    assert!(error.message.contains(EVALUATION), "{}", error.message);
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 0);
}

#[rstest]
#[case::campaign_pending(
    json!({ "status": "pending", "data": { "startTime": "2026-01-01T00:00:00Z" } }),
    "2020-01-01T00:00:00Z", "2999-01-01T00:00:00Z", Remedy::RetryLater
)]
#[case::campaign_not_started(
    Value::Null,
    "2999-01-01T00:00:00Z",
    "2999-12-01T00:00:00Z",
    Remedy::RetryLater
)]
#[case::campaign_expired(
    Value::Null,
    "2020-01-01T00:00:00Z",
    "2021-01-01T00:00:00Z",
    Remedy::Escalate
)]
#[tokio::test]
async fn test_preflight_campaign_state(
    #[case] last: Value,
    #[case] start: &str,
    #[case] end: &str,
    #[case] remedy: Remedy,
) {
    let engine = MockEngine::start().await;
    let mut spec = json!({ "startTime": start, "endTime": end, "rules": [], "scope": {} });
    if !last.is_null() {
        spec["lastEvaluationResult"] = last;
    }
    mount_full(
        &engine,
        "campaigns",
        CAMPAIGN,
        compliance_item("Campaign", CAMPAIGN, spec, "1"),
        None,
    )
    .await;

    let error = call(&engine, json!({ "target": "campaign", "name": CAMPAIGN }))
        .await
        .expect_err("refused");

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, remedy);
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 0);
}

// ---------------------------------------------------------------------------------------------
// the POST, and after it
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::post_5xx_rule("rule", 503, codes::UNKNOWN_OUTCOME, Remedy::Unknown)]
#[case::post_400_rule("rule", 400, codes::SERVER_DEFECT, Remedy::Escalate)]
#[case::post_400_scorecard("scorecard", 400, codes::CONFLICT, Remedy::RetryLater)]
#[tokio::test]
async fn test_post_failures(
    #[case] target: &str,
    #[case] status: u16,
    #[case] code: &str,
    #[case] remedy: Remedy,
) {
    let engine = MockEngine::start().await;
    let arguments = match target {
        "rule" => {
            mount_rule(&engine, None).await;
            mount_post(
                &engine,
                "rules",
                RULE,
                status,
                mock_error_body(status, "refused"),
            )
            .await;
            rule_args(json!({}))
        }
        _ => {
            mount_scorecard(&engine, vec![]).await;
            mount_post(
                &engine,
                "scorecards",
                SCORECARD,
                status,
                mock_error_body(status, "refused"),
            )
            .await;
            json!({ "target": "scorecard", "name": SCORECARD })
        }
    };

    let error = call(&engine, arguments).await.expect_err("the POST failed");

    assert_eq!((error.code, error.remedy), (code, remedy));
    assert_eq!(
        count(&engine, "POST", "/evaluate", None).await,
        1,
        "never retried"
    );
}

#[rstest]
#[tokio::test]
async fn test_after_post_a_failing_poll_hands_back_running() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rule-runs/{RUN}")))
        .respond_with(ResponseTemplate::new(503))
        .mount(engine.server())
        .await;

    let answer = call(&engine, rule_args(json!({})))
        .await
        .expect("never an error after the POST");

    assert_eq!(answer["status"], json!("running"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "RuleRun", "name": RUN })
    );
}

#[rstest]
#[tokio::test]
async fn test_after_post_poll_404_is_not_found() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rule-runs/{RUN}")))
        .respond_with(ResponseTemplate::new(404).set_body_json(mock_error_body(404, "not found")))
        .mount(engine.server())
        .await;

    let error = call(&engine, rule_args(json!({})))
        .await
        .expect_err("deleted mid-run");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::Escalate)
    );
}

#[rstest]
#[tokio::test]
async fn test_after_post_scorecard_not_located_hands_back_the_scorecard() {
    let engine = MockEngine::start().await;
    mount_scorecard(&engine, vec![]).await;
    mount_post(
        &engine,
        "scorecards",
        SCORECARD,
        202,
        json!({ "urn": "urn:x" }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/scorecard-evaluations")))
        .respond_with(ResponseTemplate::new(503))
        .mount(engine.server())
        .await;

    let answer = call(
        &engine,
        json!({ "target": "scorecard", "name": SCORECARD, "wait": 2 }),
    )
    .await
    .expect("a hand-back");

    assert_eq!(answer["status"], json!("running"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "Scorecard", "name": SCORECARD })
    );
}

#[rstest]
#[tokio::test]
async fn test_after_post_cancellation_is_not_retryable() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "pending", "data": null }), "1"),
        None,
    )
    .await;
    mount_version(&engine, "rule-runs", RUN, "1", None).await;
    let cancellation = CancellationToken::new();
    let context = mock_context_with(&engine, cancellation.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
    });

    let error = call_with(&engine, &context, rule_args(json!({})))
        .await
        .expect_err("cancelled");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CANCELLED, Remedy::Unknown)
    );
    assert!(error.message.contains(RUN), "{}", error.message);
}

/// A sample asked above the ceiling is clamped, and the clamp is echoed.
#[rstest]
#[tokio::test]
async fn test_clamps_failure_sample_clamped_echoed() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    mount_full(
        &engine,
        "rule-runs",
        RUN,
        rule_run(json!({ "status": "complete", "data": [] }), "1"),
        None,
    )
    .await;

    let answer = call(&engine, rule_args(json!({ "failure_sample": 2000 })))
        .await
        .expect("a result");

    assert_eq!(answer["failureSample"], json!(500));
}

// ---------------------------------------------------------------------------------------------
// poll_loop and progress, as pure functions
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_poll_loop_intervals_1_2_2_5() {
    let waits: Vec<Duration> = (0..5)
        .map(|iteration| interval(&INTERVALS, iteration))
        .collect();

    assert_eq!(
        waits,
        vec![
            Duration::from_millis(1_000),
            Duration::from_millis(2_000),
            Duration::from_millis(2_500),
            Duration::from_millis(2_500),
            Duration::from_millis(2_500),
        ]
    );
}

#[rstest]
#[case::start(Duration::ZERO, Duration::from_secs(20), (0, 20))]
#[case::midway(Duration::from_millis(7_400), Duration::from_secs(20), (7, 20))]
#[case::never_past_the_wait(Duration::from_secs(25), Duration::from_secs(20), (20, 20))]
fn test_progress_seconds_waited_of_the_wait(
    #[case] elapsed: Duration,
    #[case] wait: Duration,
    #[case] expected: (u32, u32),
) {
    assert_eq!(progress_point(elapsed, wait), expected);
}

// ---------------------------------------------------------------------------------------------
// locator: the campaign guard, as a pure function
// ---------------------------------------------------------------------------------------------

fn evaluation(raw: Value) -> catalog_client::models::compliance::CampaignEvaluation {
    serde_json::from_value(raw).expect("an evaluation")
}

#[rstest]
#[case::campaign_same_second_previous_run(
    Some(json!({ "status": "complete", "data": { "startTime": "2026-01-01T10:00:00Z", "endTime": "2026-01-01T10:00:00Z" } })),
    json!({ "status": "complete", "data": { "startTime": "2026-01-01T10:00:00Z", "endTime": "2026-01-01T10:00:00Z" } }),
    false
)]
#[case::older_than_the_post(
    None,
    json!({ "status": "complete", "data": { "startTime": "2026-01-01T09:59:59Z", "endTime": "2026-01-01T10:00:00Z" } }),
    false
)]
#[case::new_in_the_same_second(
    Some(json!({ "status": "complete", "data": { "startTime": "2025-12-01T00:00:00Z", "endTime": "2025-12-01T00:00:05Z" } })),
    json!({ "status": "pending", "data": { "startTime": "2026-01-01T10:00:00Z" } }),
    true
)]
#[case::first_ever(
    None,
    json!({ "status": "complete", "data": { "startTime": "2026-01-01T10:00:03Z", "endTime": "2026-01-01T10:00:04Z" } }),
    true
)]
fn test_locator_campaign_guard(
    #[case] before: Option<Value>,
    #[case] latest: Value,
    #[case] expected: bool,
) {
    let posted_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T10:00:00Z")
        .expect("a time")
        .with_timezone(&chrono::Utc);
    let before = before.map(evaluation);

    assert_eq!(
        super::is_this_calls_evaluation(before.as_ref(), &evaluation(latest), posted_at),
        expected
    );
}

// ---------------------------------------------------------------------------------------------
// scope_encoding and filter_parity
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_scope_encoding_view_by_urn() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;

    call(
        &engine,
        rule_args(json!({ "wait": 0,
            "scope": { "view": format!("urn:mia-platform-catalog:mia-platform.eu:v1:View:{VIEW}") } })),
    )
    .await
    .expect("a hand-back");

    let post = engine
        .server()
        .received_requests()
        .await
        .expect("recorded")
        .into_iter()
        .find(|request| request.method.as_str() == "POST")
        .expect("one POST");
    let body: Value = serde_json::from_slice(&post.body).expect("a JSON body");
    assert_eq!(
        body,
        json!({ "scope": { "type": "view-ref",
                           "content": format!("urn:mia-platform-catalog:mia-platform.eu:v1:View:{VIEW}") } })
    );
}

/// An unknown kind fails as it does in `search_catalog`: `not_found`, before anything runs.
#[rstest]
#[tokio::test]
async fn test_filter_parity_unknown_kind() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    Mock::given(method("GET"))
        .and(path_regex(r".*item-type-definitions$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .mount(engine.server())
        .await;

    let error = call(
        &engine,
        json!({ "target": "rule", "name": RULE, "scope": { "kind": "Nonexistent" } }),
    )
    .await
    .expect_err("no such kind");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 0);
}

/// A `spec.` filter without `kind` carries `search_catalog`'s warning.
#[rstest]
#[tokio::test]
async fn test_filter_parity_kindless_spec_warns() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;

    let answer = call(
        &engine,
        json!({ "target": "rule", "name": RULE, "wait": 0,
                "scope": { "fields": { "spec.lifecycle": "production" } } }),
    )
    .await
    .expect("a hand-back");

    assert!(
        answer["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|text| text.contains("Without `kind`"))),
        "{answer}"
    );
}

/// A caller gone before the POST is told so, and nothing is started: a retry is safe.
#[rstest]
#[tokio::test]
async fn test_cancel_before_post() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, None).await;
    mount_rule_post(&engine).await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let context = mock_context_with(&engine, cancellation);

    let error = call_with(&engine, &context, rule_args(json!({})))
        .await
        .expect_err("cancelled");

    assert_eq!(
        (error.code, error.remedy),
        (codes::CANCELLED, Remedy::Retry)
    );
    assert_eq!(count(&engine, "POST", "/evaluate", None).await, 0);
}
