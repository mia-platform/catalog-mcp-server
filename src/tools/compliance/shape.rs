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
// A run's result, shaped: what failed rather than the roll-call. A stored run carries one entry per
// item in scope; an agent asking "does it comply?" needs the counts and the failures, so that is
// what this returns — with the per-kind spread, the per-rule tally and, for a scorecard, the levels.

use super::{Target, parse_item_urn, parse_type_urn};
use catalog_client::models::compliance::{
    Evaluation, RuleOutcome, RuleRunItem, RuleRunResults, RuleRunSpec, RuleSpec, RulesItem,
    ScorecardEvaluationSpec, ScorecardSpec, TargetItemTypes, TargetMode,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// The most failing items a result lists, whatever is asked for. The counts are never cut.
pub const MAX_FAILURE_SAMPLE: u16 = 500;

/// The fewest failing items a result lists when any failed.
const MIN_FAILURE_SAMPLE: u16 = 1;

/// The level name of an item, or scorecard, that reached none — as the website shows "None".
pub const NO_LEVEL: &str = "none";

/// The `failedByKind` key of an item whose reference is not a catalog URN.
const UNKNOWN_KIND: &str = "unknown";

/// A run's state.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// Finished, with results.
    Complete,

    /// Not finished yet — or stuck; not a result.
    Running,

    /// The evaluation itself failed; there are no results.
    Error,

    /// Never evaluated.
    None,
}

/// One run's result, as `run_compliance_evaluation` ends with it and `get_compliance_results` reads
/// it. Every optional field is omitted when absent — never `null`.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct RunResult {
    /// The run's state.
    #[serde(rename = "status")]
    pub status: RunStatus,

    /// What was evaluated.
    #[serde(rename = "target")]
    pub target: Target,

    /// Its name.
    #[serde(rename = "name")]
    pub name: String,

    /// Items in scope.
    #[serde(rename = "total", skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,

    /// Items that passed.
    #[serde(rename = "passed", skip_serializing_if = "Option::is_none")]
    pub passed: Option<u64>,

    /// Items that failed, or that the rule could not be evaluated on.
    #[serde(rename = "failed", skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,

    /// Items no rule applied to.
    #[serde(rename = "skipped", skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,

    /// A rule's items outside its target types, counted apart.
    #[serde(rename = "outOfTarget", skip_serializing_if = "Option::is_none")]
    pub out_of_target: Option<u64>,

    /// The failed items, up to the sample.
    #[serde(rename = "failures", skip_serializing_if = "Option::is_none")]
    pub failures: Option<Vec<Failure>>,

    /// Whether `failures` was cut; the counts are exact either way.
    #[serde(rename = "sampled", skip_serializing_if = "std::ops::Not::not")]
    pub sampled: bool,

    /// The effective sample, only when it differs from the one asked for.
    #[serde(rename = "failureSample", skip_serializing_if = "Option::is_none")]
    pub failure_sample: Option<u16>,

    /// Every failed item, counted by `kind`.
    #[serde(rename = "failedByKind", skip_serializing_if = "Option::is_none")]
    pub failed_by_kind: Option<BTreeMap<String, u64>>,

    /// Each rule's outcomes across items: scorecards and campaigns.
    #[serde(rename = "byRule", skip_serializing_if = "Option::is_none")]
    pub by_rule: Option<BTreeMap<String, RuleTally>>,

    /// Items at each reached level, `none` included: scorecards with levels.
    #[serde(rename = "byLevel", skip_serializing_if = "Option::is_none")]
    pub by_level: Option<BTreeMap<String, u64>>,

    /// The lowest level every item reached, or `none`: scorecards with levels.
    #[serde(rename = "minimumLevel", skip_serializing_if = "Option::is_none")]
    pub minimum_level: Option<String>,

    /// Why the evaluation failed: `error` only.
    #[serde(rename = "error", skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// When the run started.
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    /// When it ended.
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,

    /// Where the result is stored.
    #[serde(rename = "resultsIn", skip_serializing_if = "Option::is_none")]
    pub results_in: Option<ResultsIn>,
}

/// One failed item.
#[derive(Serialize, Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Failure {
    /// Its name — or, for a reference that is not a catalog URN, the reference whole.
    #[serde(rename = "name")]
    pub name: String,

    /// Its `kind`.
    #[serde(rename = "kind", skip_serializing_if = "String::is_empty")]
    pub kind: String,

    /// Its group, which tells apart kinds several groups share.
    #[serde(rename = "group", skip_serializing_if = "String::is_empty")]
    pub group: String,

    /// The rules it failed: scorecards and campaigns.
    #[serde(rename = "failedRules", skip_serializing_if = "Option::is_none")]
    pub failed_rules: Option<Vec<String>>,

    /// The level it reached, or `none`: scorecards with levels.
    #[serde(rename = "level", skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

/// One rule's outcomes across items.
#[derive(Serialize, Clone, Default)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct RuleTally {
    /// Items it passed on.
    #[serde(rename = "passed")]
    pub passed: u64,

    /// Items it failed on, or could not be evaluated on.
    #[serde(rename = "failed")]
    pub failed: u64,

    /// Items it did not apply to.
    #[serde(rename = "skipped")]
    pub skipped: u64,
}

/// Where a result is stored, as `describe_item` and `get_compliance_results` take it.
#[derive(Serialize, Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct ResultsIn {
    /// `RuleRun`, `ScorecardEvaluation`, `Scorecard` or `Campaign`.
    #[serde(rename = "kind")]
    pub kind: String,

    /// Its name.
    #[serde(rename = "name")]
    pub name: String,
}

/// A completed run's counts, before they are placed in a [`RunResult`].
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct Counted {
    total: u64,
    passed: u64,
    failed: u64,
    skipped: u64,
    out_of_target: Option<u64>,
    failures: Vec<Failure>,
    sampled: bool,
    failed_by_kind: BTreeMap<String, u64>,
    by_rule: Option<BTreeMap<String, RuleTally>>,
    by_level: Option<BTreeMap<String, u64>>,
    minimum_level: Option<String>,
}

#[cfg(test)]
impl Counted {
    /// Items in scope.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Items that passed.
    pub fn passed(&self) -> u64 {
        self.passed
    }

    /// Items that failed.
    pub fn failed(&self) -> u64 {
        self.failed
    }

    /// Items no rule applied to.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// A rule's items outside its target types, when it has target types.
    pub fn out_of_target(&self) -> Option<u64> {
        self.out_of_target
    }
}

/// The sample to list, and what to echo: clamped into `1 ..= 500` rather than refused, and echoed
/// **only** when the clamp changed it — a silent clamp would let the model believe it saw all.
pub fn effective_failure_sample(requested: Option<u16>, default: u16) -> (u16, Option<u16>) {
    match requested {
        None => (default.clamp(MIN_FAILURE_SAMPLE, MAX_FAILURE_SAMPLE), None),
        Some(requested) => {
            let effective = requested.clamp(MIN_FAILURE_SAMPLE, MAX_FAILURE_SAMPLE);
            (effective, (effective != requested).then_some(effective))
        }
    }
}

impl RunResult {
    /// A result carrying nothing but its state and subject.
    pub fn new(status: RunStatus, target: Target, name: &str) -> Self {
        Self {
            status,
            target,
            name: name.to_string(),
            total: None,
            passed: None,
            failed: None,
            skipped: None,
            out_of_target: None,
            failures: None,
            sampled: false,
            failure_sample: None,
            failed_by_kind: None,
            by_rule: None,
            by_level: None,
            minimum_level: None,
            error: None,
            started_at: None,
            ended_at: None,
            results_in: None,
        }
    }

    /// A completed run's result.
    pub fn complete(target: Target, name: &str, counted: Counted) -> Self {
        Self {
            total: Some(counted.total),
            passed: Some(counted.passed),
            failed: Some(counted.failed),
            skipped: Some(counted.skipped),
            out_of_target: counted.out_of_target,
            failures: Some(counted.failures),
            sampled: counted.sampled,
            failed_by_kind: Some(counted.failed_by_kind),
            by_rule: counted.by_rule,
            by_level: counted.by_level,
            minimum_level: counted.minimum_level,
            ..Self::new(RunStatus::Complete, target, name)
        }
    }

    /// A failed evaluation: the engine's message, and no counts.
    pub fn error(target: Target, name: &str, message: &str) -> Self {
        Self {
            error: Some(message.to_string()),
            ..Self::new(RunStatus::Error, target, name)
        }
    }

    /// Sets when the run started and ended, when known.
    pub fn with_times(mut self, started_at: Option<&str>, ended_at: Option<&str>) -> Self {
        self.started_at = started_at.map(str::to_string);
        self.ended_at = ended_at.map(str::to_string);
        self
    }

    /// Sets where the result is stored.
    pub fn with_results_in(mut self, kind: &str, name: &str) -> Self {
        self.results_in = Some(ResultsIn {
            kind: kind.to_string(),
            name: name.to_string(),
        });
        self
    }

    /// Echoes the effective sample, when the clamp changed it.
    pub fn with_failure_sample_echo(mut self, echo: Option<u16>) -> Self {
        self.failure_sample = echo;
        self
    }
}

/// A rule's target types, as the `(apiVersion, kind)` pairs the policy engine matches on.
struct Targets {
    mode: TargetMode,
    types: Vec<(String, String)>,
}

impl Targets {
    /// The target types a rule declares, or `None` when it declares none it can be matched by —
    /// which is also how the policy engine treats them: no types, no restriction.
    fn of(declared: Option<&TargetItemTypes>) -> Option<Self> {
        let declared = declared?;
        let types: Vec<(String, String)> = declared
            .item_type_refs
            .iter()
            .filter_map(|urn| parse_type_urn(urn))
            .collect();

        (!types.is_empty()).then_some(Self {
            mode: declared.mode,
            types,
        })
    }

    /// Whether an item of `api_version` and `kind` is one the rule applies to — equal `apiVersion`
    /// **and** `kind`, `include` keeping the listed types, `exclude` the others, exactly as the
    /// policy engine's `Matches` does.
    fn applies_to(&self, api_version: &str, kind: &str) -> bool {
        let listed = self.types.iter().any(|(listed_version, listed_kind)| {
            listed_version == api_version && listed_kind == kind
        });

        match self.mode {
            TargetMode::Include => listed,
            TargetMode::Exclude => !listed,
        }
    }
}

/// Collects failures up to the sample, and every failure's kind.
struct FailureList {
    sample: usize,
    failures: Vec<Failure>,
    count: u64,
    by_kind: BTreeMap<String, u64>,
}

impl FailureList {
    fn new(sample: u16) -> Self {
        Self {
            sample: usize::from(sample),
            failures: Vec::new(),
            count: 0,
            by_kind: BTreeMap::new(),
        }
    }

    fn push(&mut self, item_ref: &str, failed_rules: Option<Vec<String>>, level: Option<String>) {
        let parsed = parse_item_urn(item_ref);
        let kind = parsed
            .as_ref()
            .map_or(UNKNOWN_KIND, |parsed| parsed.kind.as_str());

        self.count += 1;
        *self.by_kind.entry(kind.to_string()).or_default() += 1;

        if self.failures.len() < self.sample {
            self.failures.push(match parsed {
                Some(parsed) => Failure {
                    name: parsed.name,
                    kind: parsed.kind,
                    group: parsed.group,
                    failed_rules,
                    level,
                },
                None => Failure {
                    name: item_ref.to_string(),
                    kind: String::new(),
                    group: String::new(),
                    failed_rules,
                    level,
                },
            });
        }
    }
}

/// Counts a rule run's results.
///
/// An item is **out of target** when the rule has target types and the item's type is outside
/// them — this wins over what the engine stored, which today is `false` for such an item; otherwise
/// **passed** for `true`, **skipped** for `"skipped"`, and **failed** for anything else, an
/// evaluation error included, since the engine does not tell the two apart.
pub fn count_rule_run(
    items: &[RuleRunItem],
    target_item_types: Option<&TargetItemTypes>,
    sample: u16,
) -> Counted {
    let targets = Targets::of(target_item_types);
    let mut failures = FailureList::new(sample);
    let (mut passed, mut skipped, mut out_of_target) = (0, 0, 0);

    for item in items {
        let outside = targets.as_ref().is_some_and(|targets| {
            parse_item_urn(&item.item_ref).is_some_and(|parsed| {
                !targets.applies_to(
                    &format!("{}/{}", parsed.group, parsed.version),
                    &parsed.kind,
                )
            })
        });

        if outside {
            out_of_target += 1;
        } else if item.result.passed() {
            passed += 1;
        } else if item.result.skipped() {
            skipped += 1;
        } else {
            failures.push(&item.item_ref, None, None);
        }
    }

    Counted {
        total: items.len() as u64,
        passed,
        failed: failures.count,
        skipped,
        out_of_target: targets.map(|_| out_of_target),
        sampled: failures.count > failures.failures.len() as u64,
        failures: failures.failures,
        failed_by_kind: failures.by_kind,
        by_rule: None,
        by_level: None,
        minimum_level: None,
    }
}

/// Counts a scorecard's or a campaign's results, where each item carries one outcome per rule.
///
/// An item **failed** when any rule is `false`; otherwise it was **skipped** when every rule was
/// `"skipped"`; otherwise it **passed**. `scorecard` brings the levels; a campaign has none.
pub fn count_rules_items(
    items: &[RulesItem],
    scorecard: Option<&ScorecardSpec>,
    sample: u16,
) -> Counted {
    let levels = scorecard.and_then(Levels::of);
    let mut failures = FailureList::new(sample);
    let mut by_rule: BTreeMap<String, RuleTally> = BTreeMap::new();
    let mut by_level: BTreeMap<String, u64> = BTreeMap::new();
    let mut reached_levels: Vec<Option<usize>> = Vec::with_capacity(items.len());
    let (mut passed, mut skipped) = (0, 0);

    for item in items {
        for (rule, outcome) in &item.rules {
            let tally = by_rule.entry(rule.clone()).or_default();
            if outcome.passed() {
                tally.passed += 1;
            } else if outcome.skipped() {
                tally.skipped += 1;
            } else if outcome.failed() {
                tally.failed += 1;
            }
        }

        let reached = levels.as_ref().map(|levels| levels.reached(&item.rules));
        if let Some(reached) = reached {
            reached_levels.push(reached);
            let name = levels
                .as_ref()
                .map_or(NO_LEVEL.to_string(), |levels| levels.name_of(reached));
            *by_level.entry(name).or_default() += 1;
        }

        let failed_rules: Vec<String> = item
            .rules
            .iter()
            .filter(|(_, outcome)| outcome.failed())
            .map(|(rule, _)| rule.clone())
            .collect();

        if !failed_rules.is_empty() {
            let level = levels
                .as_ref()
                .zip(reached)
                .map(|(levels, reached)| levels.name_of(reached));
            failures.push(&item.item_ref, Some(failed_rules), level);
        } else if item.rules.values().all(RuleOutcome::skipped) {
            skipped += 1;
        } else {
            passed += 1;
        }
    }

    let minimum_level = levels.as_ref().filter(|_| !items.is_empty()).map(|levels| {
        let lowest = reached_levels
            .iter()
            .copied()
            .try_fold(usize::MAX, |lowest, reached| {
                reached.map(|reached| lowest.min(reached))
            });
        levels.name_of(lowest)
    });

    Counted {
        total: items.len() as u64,
        passed,
        failed: failures.count,
        skipped,
        out_of_target: None,
        sampled: failures.count > failures.failures.len() as u64,
        failures: failures.failures,
        failed_by_kind: failures.by_kind,
        by_rule: Some(by_rule),
        by_level: levels.map(|_| by_level),
        minimum_level,
    }
}

/// A scorecard's levels, **lowest first**, with the result keys of each level's rules.
struct Levels {
    names: Vec<String>,
    rules: Vec<Vec<String>>,
}

impl Levels {
    /// The scorecard's levels, or `None` when it has none.
    fn of(scorecard: &ScorecardSpec) -> Option<Self> {
        if scorecard.levels.is_empty() {
            return None;
        }

        let mut by_level: HashMap<&str, Vec<String>> = HashMap::new();
        for rule in &scorecard.rules {
            if let Some(level) = rule.level() {
                by_level
                    .entry(level)
                    .or_default()
                    .push(rule.result_key().to_string());
            }
        }

        // Stored highest first; walked lowest first.
        let names: Vec<String> = scorecard
            .levels
            .iter()
            .rev()
            .map(|level| level.name.clone())
            .collect();
        let rules = names
            .iter()
            .map(|name| by_level.get(name.as_str()).cloned().unwrap_or_default())
            .collect();

        Some(Self { names, rules })
    }

    /// The highest level an item reached, as an index into the lowest-first list.
    ///
    /// Walking up from the lowest, a level is reached when none of its rules is `false` and at
    /// least one of them was not `"skipped"`; the walk stops at the first level not reached — so an
    /// item reaches a level only with every level below it. The website's own rule, to the letter,
    /// including a rule with no stored outcome counting as neither failed nor skipped.
    fn reached(&self, outcomes: &BTreeMap<String, RuleOutcome>) -> Option<usize> {
        let mut reached = None;

        for (index, rules) in self.rules.iter().enumerate() {
            let none_failed = rules
                .iter()
                .all(|rule| !outcomes.get(rule).is_some_and(RuleOutcome::failed));
            let some_ran = rules
                .iter()
                .any(|rule| !outcomes.get(rule).is_some_and(RuleOutcome::skipped));

            if none_failed && some_ran {
                reached = Some(index);
            } else {
                break;
            }
        }

        reached
    }

    /// A level's name, or `none`.
    fn name_of(&self, reached: Option<usize>) -> String {
        reached
            .and_then(|index| self.names.get(index))
            .map_or_else(|| NO_LEVEL.to_string(), Clone::clone)
    }
}

/// What reading a run found: still going, or finished with its result.
pub enum Outcome {
    /// Not finished; when it started, if known.
    Pending(Option<String>),

    /// Finished — `complete` or `error` — with the result to return.
    Finished(Box<RunResult>),
}

/// Judges a RuleRun: its result read against the rule's target types.
///
/// The one place a RuleRun becomes a result, for both tools, so the two answers about the same run
/// cannot differ.
pub fn judge_rule_run(name: &str, run: RuleRunSpec, rule: &RuleSpec, sample: u16) -> Outcome {
    let (started, ended) = (run.run_start_time, run.run_end_time);

    match run.results {
        RuleRunResults::Pending {} => Outcome::Pending(started),
        RuleRunResults::Error { data } => Outcome::Finished(Box::new(
            RunResult::error(Target::Rule, name, &data.error)
                .with_times(started.as_deref(), ended.as_deref()),
        )),
        RuleRunResults::Complete { data } => Outcome::Finished(Box::new(
            RunResult::complete(
                Target::Rule,
                name,
                count_rule_run(&data, rule.target_item_types.as_ref(), sample),
            )
            .with_times(started.as_deref(), ended.as_deref()),
        )),
    }
}

/// Judges a ScorecardEvaluation: its result read against the scorecard's levels.
pub fn judge_scorecard_evaluation(
    name: &str,
    evaluation: ScorecardEvaluationSpec,
    scorecard: &ScorecardSpec,
    sample: u16,
) -> Outcome {
    match evaluation.evaluation {
        Evaluation::Pending { data } => Outcome::Pending(Some(data.start_time)),
        Evaluation::Error { data } => Outcome::Finished(Box::new(
            RunResult::error(Target::Scorecard, name, &data.error)
                .with_times(Some(&data.start_time), Some(&data.end_time)),
        )),
        Evaluation::Complete { data } => Outcome::Finished(Box::new(
            RunResult::complete(
                Target::Scorecard,
                name,
                count_rules_items(&data.results, Some(scorecard), sample),
            )
            .with_times(Some(&data.start_time), Some(&data.end_time)),
        )),
    }
}
