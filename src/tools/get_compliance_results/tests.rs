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
// public.

use crate::{
    registry::contract::{CallContext, Tool},
    tools::{
        get_compliance_results::{
            GetComplianceResults, GetComplianceResultsInput, RunScope, summarise_scope,
            updated_after,
        },
        run_compliance_evaluation::{RunComplianceEvaluation, RunComplianceEvaluationInput},
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use catalog_client::{
    CallerIdentity, Deadline, EngineClientFactory, Projection, Remedy, ToolError,
    error::codes,
    testing::{MockEngine, mock_acl_context, mock_error_body, mock_list_envelope},
};
use configuration::tools::ToolDefaults;
use rstest::rstest;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Match, Mock, Request, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

const COMPLIANCE: &str = "/compliance.mia-platform.eu/v1/items";
const RULE: &str = "my-rule";
const SCORECARD: &str = "my-card";
const CAMPAIGN: &str = "my-campaign";
const RULE_URN: &str = "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:my-rule";

/// Matches a listing whose decoded `rawq` contains `needle`.
struct RawqHas(&'static str);

impl Match for RawqHas {
    fn matches(&self, request: &Request) -> bool {
        request
            .url
            .query_pairs()
            .filter(|(key, _)| key == "rawq")
            .any(|(_, value)| {
                URL_SAFE_NO_PAD
                    .decode(value.as_bytes())
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .is_some_and(|query| query.contains(self.0))
            })
    }
}

/// What tells the two listings apart.
const FINISHED_RULE: &str = r#""exists":true"#;
const PENDING_RULE: &str = r#""exists":false"#;
const FINISHED_EVALUATION: &str = r#""or""#;
const PENDING_EVALUATION: &str = r#"{"eq":"pending"}"#;

fn mock_context(engine: &MockEngine) -> CallContext {
    let identity = Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        Some("Bearer test-token"),
        None,
    ));
    let budget = Duration::from_secs(10);
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

async fn call(engine: &MockEngine, arguments: Value) -> Result<Value, ToolError> {
    let input: GetComplianceResultsInput =
        serde_json::from_value(arguments).expect("the arguments deserialise");

    GetComplianceResults::new(&ToolDefaults::default())
        .call(&mock_context(engine), input)
        .await
        .map(|output| output.render(None))
}

fn item(kind: &str, name: &str, spec: Value, created: &str, updated: &str) -> Value {
    json!({
        "apiVersion": "compliance.mia-platform.eu/v1",
        "kind": kind,
        "metadata": {
            "name": name,
            "creationTimestamp": created,
            "updateTimestamp": updated
        },
        "spec": spec,
        "resourceVersion": "1"
    })
}

fn urn(kind: &str, name: &str) -> String {
    format!("urn:mia-platform-catalog:example.com:v1:{kind}:{name}")
}

/// A rule updated at `updated`.
async fn mount_rule(engine: &MockEngine, updated: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rules/{RULE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item(
            "Rule",
            RULE,
            json!({ "body": {} }),
            "2025-01-01T00:00:00Z",
            updated,
        )))
        .mount(engine.server())
        .await;
}

/// A finished or pending RuleRun.
fn rule_run(name: &str, started: &str, results: Value) -> Value {
    let mut spec = json!({
        "ruleRef": RULE_URN,
        "runStartTime": started,
        "scope": { "type": "view-ref", "content": "urn:mia-platform-catalog:mia-platform.eu:v1:View:my-view" },
        "results": results
    });
    if results["status"] != json!("pending") {
        spec["runEndTime"] = json!(started);
    }
    item("RuleRun", name, spec, started, started)
}

fn complete(passed: usize, failed: usize) -> Value {
    let data: Vec<Value> = (0..passed)
        .map(|index| json!({ "itemRef": urn("Service", &format!("ok-{index}")), "result": true }))
        .chain((0..failed).map(
            |index| json!({ "itemRef": urn("Service", &format!("ko-{index}")), "result": false }),
        ))
        .collect();
    json!({ "status": "complete", "data": data })
}

/// Mounts the rule-runs listing answering `runs` for the listing `needle` selects.
async fn mount_runs(engine: &MockEngine, family: &str, needle: &'static str, runs: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/{family}")))
        .and(RawqHas(needle))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(runs, None)))
        .mount(engine.server())
        .await;
}

/// The `limit` the finished listing was asked with.
async fn finished_limit(engine: &MockEngine) -> Option<String> {
    engine
        .server()
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .filter(|request| RawqHas(FINISHED_RULE).matches(request))
        .find_map(|request| {
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "limit")
                .map(|(_, value)| value.to_string())
        })
}

// ---------------------------------------------------------------------------------------------
// validation and clamps
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::name_pattern(json!({ "target": "rule", "name": "Bad_Name" }), "name")]
#[case::run_pattern(json!({ "target": "rule", "name": RULE, "run": "Bad_Run" }), "run")]
#[case::run_for_campaign(json!({ "target": "campaign", "name": CAMPAIGN, "run": "x" }), "run")]
#[case::history_for_campaign(json!({ "target": "campaign", "name": CAMPAIGN, "history": 3 }), "history")]
#[case::run_with_history(json!({ "target": "rule", "name": RULE, "run": "x", "history": 3 }), "history")]
#[tokio::test]
async fn test_validation(#[case] arguments: Value, #[case] field: &str) {
    let engine = MockEngine::start().await;

    let error = call(&engine, arguments).await.expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert_eq!(error.details.expect("details")["field"], json!(field));
}

#[rstest]
fn test_validation_unknown_field() {
    assert!(
        serde_json::from_value::<GetComplianceResultsInput>(
            json!({ "target": "rule", "name": RULE, "runs": "x" })
        )
        .is_err()
    );
}

#[rstest]
#[case::history_default(None, "6", None)]
#[case::history_zero_none(Some(0), "1", None)]
#[case::history_clamped_echoed(Some(50), "21", Some(20))]
#[tokio::test]
async fn test_clamps_history(
    #[case] history: Option<u8>,
    #[case] limit: &str,
    #[case] echo: Option<u8>,
) {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_runs(
        &engine,
        "rule-runs",
        FINISHED_RULE,
        vec![rule_run("run-1", "2026-01-02T00:00:00Z", complete(1, 0))],
    )
    .await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;

    let mut arguments = json!({ "target": "rule", "name": RULE });
    if let Some(history) = history {
        arguments["history"] = json!(history);
    }
    let answer = call(&engine, arguments).await.expect("a result");

    assert_eq!(finished_limit(&engine).await.as_deref(), Some(limit));
    assert_eq!(
        answer.get("historyLimit").cloned(),
        echo.map(|echo| json!(echo))
    );
    assert!(answer.get("history").is_none(), "one run: no earlier ones");
}

// ---------------------------------------------------------------------------------------------
// choosing_the_run and history
// ---------------------------------------------------------------------------------------------

#[rstest]
#[tokio::test]
async fn test_choosing_the_run_latest_finished_with_history() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_runs(
        &engine,
        "rule-runs",
        FINISHED_RULE,
        vec![
            rule_run("run-3", "2026-01-03T00:00:00Z", complete(2, 1)),
            rule_run(
                "run-2",
                "2026-01-02T00:00:00Z",
                json!({ "status": "error", "data": { "error": "boom" } }),
            ),
            rule_run("run-1", "2026-01-01T00:00:00Z", complete(3, 0)),
        ],
    )
    .await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "RuleRun", "name": "run-3" })
    );
    assert_eq!(answer["scope"], json!({ "view": "my-view" }));
    assert_eq!(
        answer["history"],
        json!([
            { "name": "run-2", "status": "error",
              "startedAt": "2026-01-02T00:00:00Z", "endedAt": "2026-01-02T00:00:00Z" },
            { "name": "run-1", "status": "complete",
              "startedAt": "2026-01-01T00:00:00Z", "endedAt": "2026-01-01T00:00:00Z",
              "total": 3, "passed": 3, "failed": 0 }
        ])
    );
    assert!(answer.get("newerRun").is_none());
    assert!(answer.get("updatedSinceRun").is_none());
}

#[rstest]
#[tokio::test]
async fn test_choosing_the_run_error_newer_than_complete_reported() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_runs(
        &engine,
        "rule-runs",
        FINISHED_RULE,
        vec![
            rule_run(
                "run-2",
                "2026-01-02T00:00:00Z",
                json!({ "status": "error", "data": { "error": "boom" } }),
            ),
            rule_run("run-1", "2026-01-01T00:00:00Z", complete(3, 0)),
        ],
    )
    .await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("a result");

    assert_eq!(
        answer["status"],
        json!("error"),
        "last week's pass is not today's state"
    );
    assert_eq!(answer["error"], json!("boom"));
    assert!(answer.get("total").is_none());
}

#[rstest]
#[case::pending_ahead_named_as_newer_run("2026-01-05T00:00:00Z", true)]
#[case::pending_older_than_reported_not_newer("2025-12-01T00:00:00Z", false)]
#[tokio::test]
async fn test_choosing_the_run_newer_pending(#[case] pending_started: &str, #[case] named: bool) {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_runs(
        &engine,
        "rule-runs",
        FINISHED_RULE,
        vec![rule_run("run-1", "2026-01-01T00:00:00Z", complete(1, 0))],
    )
    .await;
    mount_runs(
        &engine,
        "rule-runs",
        PENDING_RULE,
        vec![rule_run(
            "run-p",
            pending_started,
            json!({ "status": "pending", "data": null }),
        )],
    )
    .await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(
        answer.get("newerRun").cloned(),
        named.then(|| json!({ "name": "run-p", "startedAt": pending_started }))
    );
}

#[rstest]
#[tokio::test]
async fn test_choosing_the_run_only_pending_is_running() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_runs(&engine, "rule-runs", FINISHED_RULE, vec![]).await;
    mount_runs(
        &engine,
        "rule-runs",
        PENDING_RULE,
        vec![rule_run(
            "run-p",
            "2026-01-05T00:00:00Z",
            json!({ "status": "pending", "data": null }),
        )],
    )
    .await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("a result");

    assert_eq!(answer["status"], json!("running"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "RuleRun", "name": "run-p" })
    );
    assert!(answer.get("total").is_none());
}

#[rstest]
#[tokio::test]
async fn test_choosing_the_run_no_runs_is_none() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_runs(&engine, "rule-runs", FINISHED_RULE, vec![]).await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("an outcome");

    assert_eq!(
        answer,
        json!({ "status": "none", "target": "rule", "name": RULE,
                "warnings": ["It has never been evaluated; run_compliance_evaluation runs it."] })
    );
}

#[rstest]
#[tokio::test]
async fn test_history_skipped_and_out_of_target_when_non_zero() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rules/{RULE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item(
            "Rule",
            RULE,
            json!({ "body": {}, "targetItemTypes": { "mode": "include",
                    "itemTypeRefs": ["urn:mia-platform-catalog:example.com:v1:Service"] } }),
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
        )))
        .mount(engine.server())
        .await;
    mount_runs(
        &engine,
        "rule-runs",
        FINISHED_RULE,
        vec![
            rule_run("run-2", "2026-01-02T00:00:00Z", complete(1, 0)),
            rule_run(
                "run-1",
                "2026-01-01T00:00:00Z",
                json!({ "status": "complete", "data": [
                    { "itemRef": urn("Service", "a"), "result": "skipped" },
                    { "itemRef": urn("Project", "b"), "result": false }
                ] }),
            ),
        ],
    )
    .await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("a result");

    assert_eq!(answer["history"][0]["skipped"], json!(1));
    assert_eq!(answer["history"][0]["outOfTarget"], json!(1));
    assert_eq!(answer["history"][0]["failed"], json!(0));
}

/// A run made before the rule was edited says so, on the reported run and on each history row.
#[rstest]
#[tokio::test]
async fn test_history_updated_since_run_per_row() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2026-01-02T12:00:00Z").await;
    mount_runs(
        &engine,
        "rule-runs",
        FINISHED_RULE,
        vec![
            rule_run("run-3", "2026-01-03T00:00:00Z", complete(1, 0)),
            rule_run("run-1", "2026-01-01T00:00:00Z", complete(1, 0)),
        ],
    )
    .await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;

    let answer = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("a result");

    assert!(
        answer.get("updatedSinceRun").is_none(),
        "run-3 is after the edit"
    );
    assert_eq!(answer["history"][0]["updatedSinceRun"], json!(true));
}

// ---------------------------------------------------------------------------------------------
// named_run
// ---------------------------------------------------------------------------------------------

async fn mount_run(engine: &MockEngine, name: &str, body: Value, status: u16) {
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rule-runs/{name}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(engine.server())
        .await;
}

#[rstest]
#[tokio::test]
async fn test_named_run_found() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_run(
        &engine,
        "run-1",
        rule_run("run-1", "2026-01-01T00:00:00Z", complete(2, 1)),
        200,
    )
    .await;

    let answer = call(
        &engine,
        json!({ "target": "rule", "name": RULE, "run": "run-1" }),
    )
    .await
    .expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(answer["failed"], json!(1));
    assert!(answer.get("newerRun").is_none());
    assert!(answer.get("history").is_none());
}

#[rstest]
#[tokio::test]
async fn test_named_run_other_target_refused() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    let mut other = rule_run("run-x", "2026-01-01T00:00:00Z", complete(1, 0));
    other["spec"]["ruleRef"] =
        json!("urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:other-rule");
    mount_run(&engine, "run-x", other, 200).await;

    let error = call(
        &engine,
        json!({ "target": "rule", "name": RULE, "run": "run-x" }),
    )
    .await
    .expect_err("not this rule's run");

    assert_eq!(
        (error.code, error.remedy),
        (codes::INVALID_INPUT, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains("other-rule"), "{}", error.message);
}

#[rstest]
#[tokio::test]
async fn test_named_run_pending_named_is_running() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_run(
        &engine,
        "run-p",
        rule_run(
            "run-p",
            "2026-01-05T00:00:00Z",
            json!({ "status": "pending", "data": null }),
        ),
        200,
    )
    .await;

    let answer = call(
        &engine,
        json!({ "target": "rule", "name": RULE, "run": "run-p" }),
    )
    .await
    .expect("a result");

    assert_eq!(answer["status"], json!("running"));
}

#[rstest]
#[tokio::test]
async fn test_errors_run_not_found_names_history() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    mount_run(&engine, "run-z", mock_error_body(404, "not found"), 404).await;

    let error = call(
        &engine,
        json!({ "target": "rule", "name": RULE, "run": "run-z" }),
    )
    .await
    .expect_err("no such run");

    assert_eq!(
        (error.code, error.remedy),
        (codes::NOT_FOUND, Remedy::RetryAfterChange)
    );
    assert!(error.message.contains("history"), "{}", error.message);
    assert!(error.details.is_none(), "no candidates for a run");
}

#[rstest]
#[tokio::test]
async fn test_errors_a_400_on_our_listing_is_ours() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rule-runs")))
        .respond_with(ResponseTemplate::new(400).set_body_json(mock_error_body(400, "bad query")))
        .mount(engine.server())
        .await;

    let error = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect_err("refused");

    assert_eq!(
        (error.code, error.remedy),
        (codes::SERVER_DEFECT, Remedy::Escalate)
    );
}

// ---------------------------------------------------------------------------------------------
// campaign_states
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::never_evaluated(None, false, "none", false, false)]
#[case::complete(Some(json!({ "status": "complete", "data": { "startTime": "2026-01-01T00:00:00Z", "endTime": "2026-01-01T00:00:05Z" } })), true, "complete", true, false)]
#[case::error_hides_earlier_results(Some(json!({ "status": "error", "data": { "startTime": "2026-01-01T00:00:00Z", "endTime": "2026-01-01T00:00:05Z", "error": "boom" } })), true, "error", false, false)]
#[case::pending_shows_previous_results(Some(json!({ "status": "pending", "data": { "startTime": "2026-01-02T00:00:00Z" } })), true, "running", true, true)]
#[case::pending_first_run(Some(json!({ "status": "pending", "data": { "startTime": "2026-01-02T00:00:00Z" } })), false, "running", false, false)]
#[tokio::test]
async fn test_campaign_states(
    #[case] last: Option<Value>,
    #[case] with_results: bool,
    #[case] status: &str,
    #[case] counts: bool,
    #[case] previous: bool,
) {
    let engine = MockEngine::start().await;
    let mut spec = json!({ "startTime": "2020-01-01T00:00:00Z", "endTime": "2999-01-01T00:00:00Z",
                           "rules": [], "scope": {} });
    if let Some(last) = last {
        spec["lastEvaluationResult"] = last;
    }
    if with_results {
        spec["results"] = json!([ { "itemRef": urn("Project", "p"), "rules": { "goal": false } } ]);
    }
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/campaigns/{CAMPAIGN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item(
            "Campaign",
            CAMPAIGN,
            spec,
            "2020-01-01T00:00:00Z",
            "2020-01-01T00:00:00Z",
        )))
        .mount(engine.server())
        .await;

    let answer = call(&engine, json!({ "target": "campaign", "name": CAMPAIGN }))
        .await
        .expect("an outcome");

    assert_eq!(answer["status"], json!(status));
    assert_eq!(answer.get("failed").is_some(), counts);
    assert_eq!(answer.get("previousResults").is_some(), previous);
    if status == "running" {
        assert!(answer.get("endedAt").is_none());
    }
}

// ---------------------------------------------------------------------------------------------
// same_shape_as_t4
// ---------------------------------------------------------------------------------------------

/// The same finished RuleRun gives identical result fields from both tools.
#[rstest]
#[tokio::test]
async fn test_same_shape_as_t4() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    let run = rule_run("run-1", "2026-01-01T00:00:00Z", complete(2, 3));
    mount_runs(&engine, "rule-runs", FINISHED_RULE, vec![run.clone()]).await;
    mount_runs(&engine, "rule-runs", PENDING_RULE, vec![]).await;
    Mock::given(method("GET"))
        .and(path("/mia-platform.eu/v1/items/views/my-view"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "mia-platform.eu/v1", "kind": "View",
            "metadata": { "name": "my-view" }, "spec": {} })))
        .mount(engine.server())
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{COMPLIANCE}/rules/{RULE}/evaluate")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "urn": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:RuleRun:run-1" })))
        .mount(engine.server())
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rule-runs/run-1")))
        .and(header("accept", Projection::Full.accept()))
        .respond_with(ResponseTemplate::new(200).set_body_json(run))
        .mount(engine.server())
        .await;

    let read = call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("T19's answer");
    let ran: RunComplianceEvaluationInput = serde_json::from_value(
        json!({ "target": "rule", "name": RULE, "scope": { "view": "my-view" }, "wait": 2 }),
    )
    .expect("arguments");
    static FAST: [Duration; 1] = [Duration::from_millis(5)];
    let ran = RunComplianceEvaluation::with_intervals(&ToolDefaults::default(), &FAST)
        .call(&mock_context(&engine), ran)
        .await
        .expect("T4's answer")
        .render(None);

    let result_fields = [
        "status",
        "target",
        "name",
        "total",
        "passed",
        "failed",
        "skipped",
        "failures",
        "sampled",
        "failedByKind",
        "startedAt",
        "endedAt",
        "resultsIn",
    ];
    for field in result_fields {
        assert_eq!(
            read.get(field),
            ran.get(field),
            "`{field}` differs between the two tools"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// scope_summary and updated_since_run, as pure functions
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::view_ref(
    json!({ "type": "view-ref", "content": "urn:mia-platform-catalog:mia-platform.eu:v1:View:my-view" }),
    Some(RunScope::View("my-view".to_string()))
)]
#[case::json_ast_or(
    json!({ "type": "json-ast", "content": { "combinator": "or", "queries": [ { "kind": { "eq": "Service" } } ] } }),
    Some(RunScope::Filter(json!({ "combinator": "or", "queries": [ { "kind": { "eq": "Service" } } ] })))
)]
#[case::raw_query_nested(
    json!({ "type": "raw-query", "content": { "and": [ { "or": [ { "metadata.name": { "matches": "/a/i" } } ] } ] } }),
    Some(RunScope::Filter(json!({ "and": [ { "or": [ { "metadata.name": { "matches": "/a/i" } } ] } ] })))
)]
#[case::unknown_type(json!({ "type": "other", "content": {} }), None)]
fn test_scope_summary(#[case] stored: Value, #[case] expected: Option<RunScope>) {
    assert_eq!(summarise_scope(&stored), expected);
}

#[rstest]
#[case::later_set(Some("2026-01-02T00:00:01Z"), Some("2026-01-02T00:00:00Z"), true)]
#[case::same_second_not_set(Some("2026-01-02T00:00:00Z"), Some("2026-01-02T00:00:00Z"), false)]
#[case::earlier_not_set(Some("2026-01-01T00:00:00Z"), Some("2026-01-02T00:00:00Z"), false)]
#[case::unknown_not_set(None, Some("2026-01-02T00:00:00Z"), false)]
fn test_updated_since_run(
    #[case] updated: Option<&str>,
    #[case] started: Option<&str>,
    #[case] expected: bool,
) {
    assert_eq!(updated_after(updated, started), expected);
}

/// The listings are built by this server, `limit` and newest-first `sort` included.
#[rstest]
#[tokio::test]
async fn test_contract_listings_are_sorted_newest_first() {
    let engine = MockEngine::start().await;
    mount_rule(&engine, "2025-01-01T00:00:00Z").await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/rule-runs")))
        .and(query_param("sort", "-metadata.creationTimestamp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_list_envelope(vec![], None)))
        .expect(2)
        .mount(engine.server())
        .await;

    call(&engine, json!({ "target": "rule", "name": RULE }))
        .await
        .expect("an outcome");
}

/// A scorecard: finished evaluations by `complete or error`, levels read from its definition, and
/// a newer pending evaluation named.
#[rstest]
#[tokio::test]
async fn test_choosing_the_run_scorecard_with_levels_and_a_newer_pending() {
    let engine = MockEngine::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{COMPLIANCE}/scorecards/{SCORECARD}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item(
            "Scorecard",
            SCORECARD,
            json!({ "levels": [ { "name": "gold" } ],
                    "rules": [ { "type": "inline", "content": { "name": "r" }, "level": "gold" } ],
                    "scope": {} }),
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
        )))
        .mount(engine.server())
        .await;
    let evaluation = |name: &str, created: &str, evaluation: Value| {
        item(
            "ScorecardEvaluation",
            name,
            json!({ "scorecardRef": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Scorecard:my-card",
                    "evaluation": evaluation }),
            created,
            created,
        )
    };
    mount_runs(
        &engine,
        "scorecard-evaluations",
        FINISHED_EVALUATION,
        vec![evaluation(
            "my-card-evaluation-1",
            "2026-01-01T00:00:00Z",
            json!({ "status": "complete", "data": { "startTime": "2026-01-01T00:00:00Z",
                    "endTime": "2026-01-01T00:00:03Z",
                    "results": [ { "itemRef": urn("Service", "a"), "rules": { "r": true } } ] } }),
        )],
    )
    .await;
    mount_runs(
        &engine,
        "scorecard-evaluations",
        PENDING_EVALUATION,
        vec![evaluation(
            "my-card-evaluation-2",
            "2026-01-02T00:00:00Z",
            json!({ "status": "pending", "data": { "startTime": "2026-01-02T00:00:00Z" } }),
        )],
    )
    .await;

    let answer = call(&engine, json!({ "target": "scorecard", "name": SCORECARD }))
        .await
        .expect("a result");

    assert_eq!(answer["status"], json!("complete"));
    assert_eq!(answer["minimumLevel"], json!("gold"));
    assert_eq!(
        answer["resultsIn"],
        json!({ "kind": "ScorecardEvaluation", "name": "my-card-evaluation-1" })
    );
    assert_eq!(
        answer["newerRun"],
        json!({ "name": "my-card-evaluation-2", "startedAt": "2026-01-02T00:00:00Z" })
    );
    assert!(
        answer.get("scope").is_none(),
        "a scorecard's scope is its own, not the run's"
    );
}
