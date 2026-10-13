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
// Synthetic copies of the shapes the engine stores; no tenant data.

use crate::models::compliance::{
    CampaignEvaluation, CampaignSpec, Evaluation, RuleOutcome, RuleRunResults, RuleRunSpec,
    RuleSpec, ScorecardEvaluationSpec, ScorecardSpec, TargetMode,
};
use rstest::rstest;
use serde_json::json;

#[rstest]
#[case::bool_true(json!(true), true, false, false)]
#[case::bool_false(json!(false), false, true, false)]
#[case::skipped(json!("skipped"), false, false, true)]
#[case::other_text(json!("other"), false, false, false)]
fn test_a_rule_outcome_reads_all_three_values(
    #[case] raw: serde_json::Value,
    #[case] passed: bool,
    #[case] failed: bool,
    #[case] skipped: bool,
) {
    let outcome: RuleOutcome = serde_json::from_value(raw).expect("an outcome");

    assert_eq!(
        (outcome.passed(), outcome.failed(), outcome.skipped()),
        (passed, failed, skipped)
    );
}

#[rstest]
#[case::pending(json!({ "status": "pending", "data": null }))]
#[case::error(json!({ "status": "error", "data": { "error": "Failed to fetch scope items" } }))]
#[case::complete(json!({ "status": "complete", "data": [
    { "itemRef": "urn:mia-platform-catalog:example.com:v1:Service:svc-a", "result": true },
    { "itemRef": "urn:mia-platform-catalog:example.com:v1:Service:svc-b", "result": "skipped" }
] }))]
fn test_a_rule_run_spec_parses_every_status(#[case] results: serde_json::Value) {
    let spec: RuleRunSpec = serde_json::from_value(json!({
        "ruleRef": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:my-rule",
        "runStartTime": "2026-01-01T00:00:00Z",
        "scope": { "type": "view-ref", "content": "urn:mia-platform-catalog:mia-platform.eu:v1:View:my-view" },
        "results": results
    }))
    .expect("a RuleRun spec");

    assert!(spec.run_end_time.is_none());
    match spec.results {
        RuleRunResults::Pending {} => {}
        RuleRunResults::Error { data } => assert_eq!(data.error, "Failed to fetch scope items"),
        RuleRunResults::Complete { data } => {
            assert_eq!(data.len(), 2);
            assert!(data[1].result.skipped());
        }
    }
}

#[rstest]
fn test_a_scorecard_evaluation_keeps_each_rules_outcome() {
    let spec: ScorecardEvaluationSpec = serde_json::from_value(json!({
        "scorecardRef": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Scorecard:my-card",
        "evaluation": { "status": "complete", "data": {
            "startTime": "2026-01-01T00:00:00Z", "endTime": "2026-01-01T00:00:03Z",
            "results": [ { "itemRef": "urn:mia-platform-catalog:example.com:v1:Project:p",
                           "rules": { "gold-rule": "skipped", "silver-rule": false } } ]
        } }
    }))
    .expect("a ScorecardEvaluation spec");

    let Evaluation::Complete { data } = spec.evaluation else {
        panic!("a complete evaluation");
    };
    assert!(data.results[0].rules["gold-rule"].skipped());
    assert!(data.results[0].rules["silver-rule"].failed());
}

#[rstest]
#[case::never_evaluated(json!({}), None)]
#[case::pending(json!({ "lastEvaluationResult": { "status": "pending", "data": { "startTime": "2026-01-02T00:00:00Z" } } }), Some("2026-01-02T00:00:00Z"))]
#[case::error(json!({ "lastEvaluationResult": { "status": "error", "data": { "startTime": "2026-01-03T00:00:00Z", "endTime": "2026-01-03T00:00:01Z", "error": "boom" } } }), Some("2026-01-03T00:00:00Z"))]
#[case::complete(json!({ "lastEvaluationResult": { "status": "complete", "data": { "startTime": "2026-01-04T00:00:00Z", "endTime": "2026-01-04T00:00:05Z" } } }), Some("2026-01-04T00:00:00Z"))]
fn test_a_campaign_spec_reads_its_last_evaluation(
    #[case] extra: serde_json::Value,
    #[case] started: Option<&str>,
) {
    let mut raw = json!({
        "startTime": "2026-01-01T00:00:00.000+02:00",
        "endTime": "2029-01-01T00:00:00.000+02:00",
        "rules": [], "scope": {},
        "results": [ { "itemRef": "urn:mia-platform-catalog:example.com:v1:Project:p", "rules": { "goal": true } } ]
    });
    raw.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());

    let spec: CampaignSpec = serde_json::from_value(raw).expect("a Campaign spec");

    assert_eq!(
        spec.last_evaluation_result
            .as_ref()
            .map(CampaignEvaluation::start_time),
        started
    );
    assert_eq!(spec.results.map(|results| results.len()), Some(1));
}

#[rstest]
fn test_a_rule_spec_reads_its_target_types() {
    let spec: RuleSpec = serde_json::from_value(json!({
        "body": { "type": "cel", "content": "true" },
        "targetItemTypes": { "mode": "exclude",
                             "itemTypeRefs": ["urn:mia-platform-catalog:example.com:v1:Service"] }
    }))
    .expect("a Rule spec");

    let targets = spec.target_item_types.expect("target types");
    assert_eq!(targets.mode, TargetMode::Exclude);
    assert_eq!(targets.item_type_refs.len(), 1);
}

#[rstest]
fn test_a_scorecard_rule_is_keyed_by_its_own_or_the_referenced_name() {
    let spec: ScorecardSpec = serde_json::from_value(json!({
        "levels": [ { "name": "gold", "color": "#d9af44" }, { "name": "silver" } ],
        "rules": [
            { "type": "inline", "content": { "name": "inline-rule", "body": {} }, "level": "gold" },
            { "type": "rule-ref", "content": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:shared-rule", "level": "silver" },
            { "type": "inline", "content": { "name": "standalone", "body": {} } }
        ],
        "scope": {}
    }))
    .expect("a Scorecard spec");

    let keys: Vec<(&str, Option<&str>)> = spec
        .rules
        .iter()
        .map(|rule| (rule.result_key(), rule.level()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("inline-rule", Some("gold")),
            ("shared-rule", Some("silver")),
            ("standalone", None)
        ]
    );
    assert_eq!(spec.levels[0].name, "gold");
}
