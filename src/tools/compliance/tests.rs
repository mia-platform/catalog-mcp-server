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
// The shaping rules, on synthetic fixtures shaped like the runs the engine stores — fictional
// names only; the repository is public.

use crate::tools::compliance::{
    ItemRef, Target, parse_item_urn, parse_type_urn,
    shape::{
        NO_LEVEL, RunResult, RunStatus, count_rule_run, count_rules_items, effective_failure_sample,
    },
};
use catalog_client::models::compliance::{
    RuleOutcome, RuleRunItem, RulesItem, ScorecardSpec, TargetItemTypes,
};
use rstest::rstest;
use serde_json::{Value, json};

/// An item URN of the fictional `example.com` group.
fn mock_urn(kind: &str, name: &str) -> String {
    format!("urn:mia-platform-catalog:example.com:v1:{kind}:{name}")
}

/// A rule run's item.
fn mock_rule_item(kind: &str, name: &str, result: Value) -> RuleRunItem {
    RuleRunItem {
        item_ref: mock_urn(kind, name),
        result: serde_json::from_value(result).expect("an outcome"),
    }
}

/// A scorecard or campaign item.
fn mock_rules_item(name: &str, rules: &[(&str, Value)]) -> RulesItem {
    RulesItem {
        item_ref: mock_urn("Service", name),
        rules: rules
            .iter()
            .map(|(rule, outcome)| {
                (
                    rule.to_string(),
                    serde_json::from_value::<RuleOutcome>(outcome.clone()).expect("an outcome"),
                )
            })
            .collect(),
    }
}

/// A rule run of `count` Services, the first `failed` of which failed.
fn mock_rule_run(count: usize, failed: usize) -> Vec<RuleRunItem> {
    (0..count)
        .map(|index| {
            mock_rule_item(
                "Service",
                &format!("svc-{index:03}"),
                json!(index >= failed),
            )
        })
        .collect()
}

/// Target types for `Service` of `example.com/v1`.
fn mock_targets(mode: &str) -> TargetItemTypes {
    serde_json::from_value(json!({
        "mode": mode,
        "itemTypeRefs": ["urn:mia-platform-catalog:example.com:v1:Service"]
    }))
    .expect("target types")
}

/// A scorecard with levels gold above silver above bronze, one inline rule each.
fn mock_scorecard() -> ScorecardSpec {
    serde_json::from_value(json!({
        "levels": [ { "name": "gold" }, { "name": "silver" }, { "name": "bronze" } ],
        "rules": [
            { "type": "inline", "content": { "name": "rule-gold" }, "level": "gold" },
            { "type": "inline", "content": { "name": "rule-silver" }, "level": "silver" },
            { "type": "inline", "content": { "name": "rule-bronze" }, "level": "bronze" }
        ]
    }))
    .expect("a scorecard")
}

/// The result, as the model reads it.
fn rendered(result: &RunResult) -> Value {
    serde_json::to_value(result).expect("a result renders")
}

// ---------------------------------------------------------------------------------------------
// URNs and the target
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::item(
    "urn:mia-platform-catalog:example.com:v1:Service:svc-a",
    Some(ItemRef { group: "example.com".into(), version: "v1".into(), kind: "Service".into(), name: "svc-a".into() })
)]
#[case::dotted_name(
    "urn:mia-platform-catalog:example.com:v1:Service:svc.a-b",
    Some(ItemRef { group: "example.com".into(), version: "v1".into(), kind: "Service".into(), name: "svc.a-b".into() })
)]
#[case::type_urn_is_not_an_item("urn:mia-platform-catalog:example.com:v1:Service", None)]
#[case::other_scheme("urn:other:example.com:v1:Service:svc-a", None)]
#[case::empty_segment("urn:mia-platform-catalog:example.com::Service:svc-a", None)]
fn test_an_item_urn_is_a_four_part_split(#[case] urn: &str, #[case] expected: Option<ItemRef>) {
    assert_eq!(parse_item_urn(urn), expected);
}

#[rstest]
#[case::type_urn(
    "urn:mia-platform-catalog:example.com:v1:Service",
    Some(("example.com/v1".to_string(), "Service".to_string()))
)]
#[case::item_urn_is_not_a_type("urn:mia-platform-catalog:example.com:v1:Service:svc-a", None)]
fn test_a_type_urn_is_its_api_version_and_kind(
    #[case] urn: &str,
    #[case] expected: Option<(String, String)>,
) {
    assert_eq!(parse_type_urn(urn), expected);
}

#[rstest]
fn test_a_target_names_its_families_and_urn() {
    assert_eq!(Target::Rule.run_family(), Some("rule-runs"));
    assert_eq!(
        Target::Scorecard.run_family(),
        Some("scorecard-evaluations")
    );
    assert_eq!(Target::Campaign.run_family(), None);
    assert_eq!(
        Target::Scorecard.urn("my-card"),
        "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Scorecard:my-card"
    );
    assert_eq!(
        serde_json::to_value(Target::Campaign).expect("a target renders"),
        json!("campaign")
    );
}

// ---------------------------------------------------------------------------------------------
// The sample
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::default(None, 50, (50, None))]
#[case::within(Some(10), 50, (10, None))]
#[case::at_the_ceiling(Some(500), 50, (500, None))]
#[case::clamped_down_and_echoed(Some(2_000), 50, (500, Some(500)))]
#[case::clamped_up_and_echoed(Some(0), 50, (1, Some(1)))]
fn test_the_failure_sample_is_clamped_and_echoed_only_when_changed(
    #[case] requested: Option<u16>,
    #[case] default: u16,
    #[case] expected: (u16, Option<u16>),
) {
    assert_eq!(effective_failure_sample(requested, default), expected);
}

// ---------------------------------------------------------------------------------------------
// shaping
// ---------------------------------------------------------------------------------------------

/// The 500-item rule run: counts exact, failures sampled, every failure's kind counted.
#[rstest]
fn test_shaping_rule_500_items() {
    let counted = count_rule_run(&mock_rule_run(500, 20), None, 5);
    let result = rendered(&RunResult::complete(Target::Rule, "my-rule", counted));

    assert_eq!(result["total"], json!(500));
    assert_eq!(result["passed"], json!(480));
    assert_eq!(result["failed"], json!(20));
    assert_eq!(result["skipped"], json!(0));
    assert_eq!(result["failures"].as_array().map(Vec::len), Some(5));
    assert_eq!(result["sampled"], json!(true));
    assert_eq!(result["failedByKind"], json!({ "Service": 20 }));
    assert_eq!(
        result["failures"][0],
        json!({ "name": "svc-000", "kind": "Service", "group": "example.com" })
    );
    assert!(
        result.get("outOfTarget").is_none(),
        "no target types, no field"
    );
}

/// The size claim of the plan, as a byte golden: 500 items shape to well under a kilobyte.
#[rstest]
fn test_shaping_rule_500_items_byte_golden() {
    let counted = count_rule_run(&mock_rule_run(500, 20), None, 50);
    let bytes = serde_json::to_vec(&RunResult::complete(Target::Rule, "my-rule", counted))
        .expect("a result renders")
        .len();

    // 500 raw items are ~86 KB; the shaped answer lists 20 failures.
    assert!(bytes < 2_000, "the shaped result is {bytes} B");
}

#[rstest]
fn test_shaping_sampled_counts_exact() {
    let counted = count_rule_run(&mock_rule_run(100, 100), None, 1);

    assert_eq!(counted.failed(), 100);
    let result = rendered(&RunResult::complete(Target::Rule, "my-rule", counted));
    assert_eq!(result["failures"].as_array().map(Vec::len), Some(1));
    assert_eq!(result["sampled"], json!(true));
}

#[rstest]
fn test_shaping_not_sampled_omits_the_flag() {
    let counted = count_rule_run(&mock_rule_run(10, 2), None, 50);
    let result = rendered(&RunResult::complete(Target::Rule, "my-rule", counted));

    assert!(result.get("sampled").is_none());
}

#[rstest]
fn test_shaping_skipped_string_parses_and_is_not_a_failure() {
    let items = vec![
        mock_rule_item("Service", "a", json!(true)),
        mock_rule_item("Service", "b", json!("skipped")),
        mock_rule_item("Service", "c", json!(false)),
    ];
    let counted = count_rule_run(&items, None, 50);

    assert_eq!(
        (counted.passed(), counted.skipped(), counted.failed()),
        (1, 1, 1)
    );
}

#[rstest]
fn test_shaping_failed_by_kind_sums_to_failed() {
    let items = vec![
        mock_rule_item("Service", "a", json!(false)),
        mock_rule_item("Project", "b", json!(false)),
        mock_rule_item("Service", "c", json!(false)),
        mock_rule_item("Service", "d", json!(true)),
    ];
    let result = rendered(&RunResult::complete(
        Target::Rule,
        "my-rule",
        count_rule_run(&items, None, 1),
    ));

    assert_eq!(
        result["failedByKind"],
        json!({ "Project": 1, "Service": 2 })
    );
}

#[rstest]
fn test_shaping_a_reference_that_is_not_a_urn_is_listed_whole() {
    let items = vec![RuleRunItem {
        item_ref: "not-a-urn".to_string(),
        result: RuleOutcome::Bool(false),
    }];
    let result = rendered(&RunResult::complete(
        Target::Rule,
        "my-rule",
        count_rule_run(&items, None, 50),
    ));

    assert_eq!(result["failures"][0], json!({ "name": "not-a-urn" }));
    assert_eq!(result["failedByKind"], json!({ "unknown": 1 }));
}

#[rstest]
fn test_shaping_scorecard_item_fails_on_any_false_and_names_the_rules() {
    let items = vec![
        mock_rules_item("a", &[("r1", json!(true)), ("r2", json!(false))]),
        mock_rules_item("b", &[("r1", json!(true)), ("r2", json!(true))]),
        mock_rules_item("c", &[("r1", json!("skipped")), ("r2", json!("skipped"))]),
        mock_rules_item("d", &[("r1", json!(true)), ("r2", json!("skipped"))]),
    ];
    let counted = count_rules_items(&items, None, 50);

    assert_eq!(
        (counted.passed(), counted.failed(), counted.skipped()),
        (2, 1, 1)
    );
    let result = rendered(&RunResult::complete(
        Target::Campaign,
        "my-campaign",
        counted,
    ));
    assert_eq!(result["failures"][0]["failedRules"], json!(["r2"]));
    assert_eq!(
        result["byRule"],
        json!({
            "r1": { "passed": 3, "failed": 0, "skipped": 1 },
            "r2": { "passed": 1, "failed": 1, "skipped": 2 }
        })
    );
    assert!(result.get("byLevel").is_none(), "a campaign has no levels");
}

/// A rule removed from the scorecard since the run is still counted in `byRule`, and belongs to
/// no level.
#[rstest]
fn test_shaping_unknown_rule_key() {
    let items = vec![mock_rules_item(
        "a",
        &[
            ("rule-bronze", json!(true)),
            ("rule-silver", json!(true)),
            ("rule-gold", json!(true)),
            ("removed-rule", json!(false)),
        ],
    )];
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&items, Some(&mock_scorecard()), 50),
    ));

    assert_eq!(result["byRule"]["removed-rule"]["failed"], json!(1));
    assert_eq!(result["failures"][0]["level"], json!("gold"));
}

/// The invariant `passed + failed + skipped + outOfTarget = total`, for both shapes.
#[rstest]
fn test_shaping_the_counts_add_up() {
    let rule = count_rule_run(
        &[
            mock_rule_item("Service", "a", json!(true)),
            mock_rule_item("Project", "b", json!(false)),
            mock_rule_item("Service", "c", json!("skipped")),
            mock_rule_item("Service", "d", json!(false)),
        ],
        Some(&mock_targets("include")),
        50,
    );
    assert_eq!(
        rule.passed() + rule.failed() + rule.skipped() + rule.out_of_target().unwrap_or(0),
        rule.total()
    );

    let card = count_rules_items(
        &[
            mock_rules_item("a", &[("r", json!(true))]),
            mock_rules_item("b", &[("r", json!(false))]),
            mock_rules_item("c", &[("r", json!("skipped"))]),
        ],
        None,
        50,
    );
    assert_eq!(card.passed() + card.failed() + card.skipped(), card.total());
}

// ---------------------------------------------------------------------------------------------
// levels
// ---------------------------------------------------------------------------------------------

/// An item's level, given its outcome on bronze, silver and gold.
fn level_of(bronze: Value, silver: Value, gold: Value) -> Value {
    let items = vec![mock_rules_item(
        "a",
        &[
            ("rule-bronze", bronze),
            ("rule-silver", silver),
            ("rule-gold", gold),
        ],
    )];
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&items, Some(&mock_scorecard()), 50),
    ));

    result["minimumLevel"].clone()
}

#[rstest]
#[case::all_passed(json!(true), json!(true), json!(true), json!("gold"))]
#[case::cumulative_gold_needs_silver(json!(true), json!(false), json!(true), json!("bronze"))]
#[case::nothing_without_bronze(json!(false), json!(true), json!(true), json!(NO_LEVEL))]
#[case::all_skipped_level_not_reached(json!(true), json!("skipped"), json!(true), json!("bronze"))]
fn test_levels_cumulative(
    #[case] bronze: Value,
    #[case] silver: Value,
    #[case] gold: Value,
    #[case] expected: Value,
) {
    assert_eq!(level_of(bronze, silver, gold), expected);
}

#[rstest]
fn test_levels_minimum_lowest_reached_and_by_level() {
    let items = vec![
        mock_rules_item(
            "gold-item",
            &[
                ("rule-bronze", json!(true)),
                ("rule-silver", json!(true)),
                ("rule-gold", json!(true)),
            ],
        ),
        mock_rules_item(
            "silver-item",
            &[
                ("rule-bronze", json!(true)),
                ("rule-silver", json!(true)),
                ("rule-gold", json!(false)),
            ],
        ),
    ];
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&items, Some(&mock_scorecard()), 50),
    ));

    assert_eq!(result["minimumLevel"], json!("silver"));
    assert_eq!(result["byLevel"], json!({ "gold": 1, "silver": 1 }));
    assert_eq!(result["failures"][0]["level"], json!("silver"));
}

#[rstest]
fn test_levels_minimum_none_when_one_item_reached_none() {
    let items = vec![
        mock_rules_item("good", &[("rule-bronze", json!(true))]),
        mock_rules_item("bad", &[("rule-bronze", json!(false))]),
    ];
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&items, Some(&mock_scorecard()), 50),
    ));

    assert_eq!(result["minimumLevel"], json!(NO_LEVEL));
    assert_eq!(result["byLevel"]["none"], json!(1));
}

/// A `rule-ref` result is keyed by the referenced Rule's name — confirmed on the engine's data.
#[rstest]
fn test_levels_rule_ref_keyed_by_rule_name() {
    let scorecard: ScorecardSpec = serde_json::from_value(json!({
        "levels": [ { "name": "gold" } ],
        "rules": [ { "type": "rule-ref",
                     "content": "urn:mia-platform-catalog:compliance.mia-platform.eu:v1:Rule:shared-rule",
                     "level": "gold" } ]
    }))
    .expect("a scorecard");
    let items = vec![mock_rules_item("a", &[("shared-rule", json!(true))])];
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&items, Some(&scorecard), 50),
    ));

    assert_eq!(result["minimumLevel"], json!("gold"));
}

#[rstest]
fn test_levels_a_scorecard_without_levels_has_no_level_fields() {
    let scorecard: ScorecardSpec =
        serde_json::from_value(json!({ "rules": [] })).expect("a scorecard");
    let items = vec![mock_rules_item("a", &[("r", json!(false))])];
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&items, Some(&scorecard), 50),
    ));

    assert!(result.get("byLevel").is_none());
    assert!(result.get("minimumLevel").is_none());
    assert!(result["failures"][0].get("level").is_none());
}

// ---------------------------------------------------------------------------------------------
// out_of_target
// ---------------------------------------------------------------------------------------------

/// A mixed scope: two Services, two Projects, all saved `false` by the engine as today.
fn mock_mixed_scope(result: Value) -> Vec<RuleRunItem> {
    vec![
        mock_rule_item("Service", "svc-a", json!(false)),
        mock_rule_item("Service", "svc-b", json!(true)),
        mock_rule_item("Project", "prj-a", result.clone()),
        mock_rule_item("Project", "prj-b", result),
    ]
}

#[rstest]
#[case::include_engine_saved_false("include", json!(false), 2, 1)]
#[case::include_engine_saved_skipped("include", json!("skipped"), 2, 1)]
fn test_out_of_target_include(
    #[case] mode: &str,
    #[case] stored: Value,
    #[case] expected_out: u64,
    #[case] expected_failed: u64,
) {
    let counted = count_rule_run(&mock_mixed_scope(stored), Some(&mock_targets(mode)), 50);

    assert_eq!(counted.out_of_target(), Some(expected_out));
    assert_eq!(counted.failed(), expected_failed);
    let result = rendered(&RunResult::complete(Target::Rule, "my-rule", counted));
    assert!(
        result["failures"]
            .as_array()
            .expect("failures")
            .iter()
            .all(|failure| failure["kind"] == json!("Service")),
        "no out-of-target item is listed as a failure"
    );
}

#[rstest]
fn test_out_of_target_exclude() {
    let counted = count_rule_run(
        &mock_mixed_scope(json!(false)),
        Some(&mock_targets("exclude")),
        50,
    );

    // Services are excluded: both are out of target; the Projects are evaluated and failed.
    assert_eq!(counted.out_of_target(), Some(2));
    assert_eq!(counted.failed(), 2);
}

/// A type matches only on equal `apiVersion` **and** `kind`.
#[rstest]
fn test_out_of_target_match_needs_api_version_and_kind() {
    let items = vec![RuleRunItem {
        item_ref: "urn:mia-platform-catalog:other.example.com:v1:Service:svc-a".to_string(),
        result: RuleOutcome::Bool(false),
    }];
    let counted = count_rule_run(&items, Some(&mock_targets("include")), 50);

    assert_eq!(counted.out_of_target(), Some(1));
}

#[rstest]
fn test_out_of_target_no_target_types_no_field_and_zero_shown() {
    let without = count_rule_run(&mock_rule_run(3, 1), None, 50);
    assert_eq!(without.out_of_target(), None);

    let with = count_rule_run(&mock_rule_run(3, 1), Some(&mock_targets("include")), 50);
    assert_eq!(with.out_of_target(), Some(0));
    let result = rendered(&RunResult::complete(Target::Rule, "my-rule", with));
    assert_eq!(result["outOfTarget"], json!(0));
}

// ---------------------------------------------------------------------------------------------
// statuses
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_statuses_error_carries_no_counts() {
    let result = rendered(
        &RunResult::error(Target::Scorecard, "my-card", "Failed to fetch scope items")
            .with_times(Some("2026-01-01T00:00:00Z"), Some("2026-01-01T00:00:01Z"))
            .with_results_in("ScorecardEvaluation", "my-card-evaluation-1"),
    );

    assert_eq!(
        result,
        json!({
            "status": "error", "target": "scorecard", "name": "my-card",
            "error": "Failed to fetch scope items",
            "startedAt": "2026-01-01T00:00:00Z", "endedAt": "2026-01-01T00:00:01Z",
            "resultsIn": { "kind": "ScorecardEvaluation", "name": "my-card-evaluation-1" }
        })
    );
}

#[rstest]
fn test_statuses_running_carries_no_counts_and_no_nulls() {
    let result = rendered(
        &RunResult::new(RunStatus::Running, Target::Rule, "my-rule")
            .with_times(Some("2026-01-01T00:00:00Z"), None)
            .with_results_in("RuleRun", "run-my-rule-1"),
    );

    assert_eq!(
        result,
        json!({
            "status": "running", "target": "rule", "name": "my-rule",
            "startedAt": "2026-01-01T00:00:00Z",
            "resultsIn": { "kind": "RuleRun", "name": "run-my-rule-1" }
        })
    );
}

#[rstest]
fn test_statuses_zero_scope_is_complete_with_total_zero() {
    let result = rendered(&RunResult::complete(
        Target::Rule,
        "my-rule",
        count_rule_run(&[], None, 50),
    ));

    assert_eq!(result["total"], json!(0));
    assert_eq!(result["failures"], json!([]));
    assert_eq!(result["failedByKind"], json!({}));
}

#[rstest]
fn test_statuses_the_level_fields_of_an_empty_scorecard_run() {
    let result = rendered(&RunResult::complete(
        Target::Scorecard,
        "my-card",
        count_rules_items(&[], Some(&mock_scorecard()), 50),
    ));

    assert_eq!(result["byLevel"], json!({}));
    assert!(result.get("minimumLevel").is_none(), "no item, no minimum");
}
