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
    registry::{
        ToolDescriptor,
        contract::{CallContext, Tool, ToolOutput},
    },
    tools::{
        compliance::{
            EARLIER_RESULTS_NOT_SHOWN_WARNING, NEVER_EVALUATED_WARNING, PREVIOUS_RESULTS_WARNING,
            Target, ZERO_SCOPE_WARNING, parse_item_urn, parse_time, running_warning,
            runs::{self, Read, RunCondition},
            shape::{
                Outcome, RunResult, RunStatus, count_rules_items, effective_failure_sample,
                judge_rule_run, judge_scorecard_evaluation,
            },
        },
        filters,
    },
};
use catalog_client::{
    Remedy, ToolError,
    error::codes,
    is_valid_name,
    models::{
        Item,
        compliance::{
            CampaignEvaluation, CampaignSpec, RuleRunSpec, RuleSpec, ScorecardEvaluationSpec,
            ScorecardSpec,
        },
    },
};
use configuration::tools::ToolDefaults;
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "get_compliance_results";

/// What the tool does. Names every argument and the outputs that decide the next step.
const TOOL_DESCRIPTION: &str = "Reads compliance results without running anything (`target`, \
     `name`): the latest finished run's `status` (complete, error, running, none), its counts, \
     `failures` (up to `failure_sample`), `failedByKind`, and for scorecards `byRule`, `byLevel` \
     and `minimumLevel`; `newerRun` when a newer one is still running; `updatedSinceRun` when the \
     rule or scorecard changed after it. `history` (default 5) lists earlier runs with counts; \
     `run` reads one of them. Campaigns keep only their latest result. To start a run, use \
     run_compliance_evaluation.";

/// Earlier runs listed when `history` is not given.
const DEFAULT_HISTORY: u8 = 5;

/// The most earlier runs listed, whatever is asked for.
const MAX_HISTORY: u8 = 20;

/// The scheme a View URN starts with, for the scope summary.
const VIEW_URN_KIND: &str = "View";

/// Arguments for `get_compliance_results`. Conventions as `SearchCatalogInput`: unknown fields
/// refused, no `#[serde(default)]`.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct GetComplianceResultsInput {
    /// Whose results: rule, scorecard or campaign.
    #[serde(rename = "target")]
    pub target: Target,

    /// Its name.
    #[serde(rename = "name")]
    pub name: String,

    /// A run's name, instead of the latest. Not for campaigns.
    #[serde(rename = "run")]
    pub run: Option<String>,

    /// Earlier runs to list. Default 5, clamped to 20; 0 for none. Not for campaigns.
    #[serde(rename = "history")]
    pub history: Option<u8>,

    /// Failures to list. Default 50, clamped to 500; counts stay exact.
    #[serde(rename = "failure_sample")]
    pub failure_sample: Option<u16>,
}

/// T4's result fields, plus what only a reader needs. Every optional field omitted when absent.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct GetComplianceResultsOutput {
    /// The run's result, exactly as `run_compliance_evaluation` gives it.
    #[serde(flatten)]
    pub result: RunResult,

    /// A campaign's earlier results, shown while a newer evaluation is pending.
    #[serde(rename = "previousResults", skip_serializing_if = "std::ops::Not::not")]
    pub previous_results: bool,

    /// A rule run's own scope: runs of one rule may cover different items.
    #[serde(rename = "scope", skip_serializing_if = "Option::is_none")]
    pub scope: Option<RunScope>,

    /// Whether the rule or scorecard was updated — in any way — after the run started.
    #[serde(rename = "updatedSinceRun", skip_serializing_if = "std::ops::Not::not")]
    pub updated_since_run: bool,

    /// A run newer than the reported one, still pending.
    #[serde(rename = "newerRun", skip_serializing_if = "Option::is_none")]
    pub newer_run: Option<NewerRun>,

    /// The finished runs before the reported one, newest first.
    #[serde(rename = "history", skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<HistoryEntry>>,

    /// The effective `history`, only when the clamp changed it.
    #[serde(rename = "historyLimit", skip_serializing_if = "Option::is_none")]
    pub history_limit: Option<u8>,
}

/// A pending run newer than the reported one.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct NewerRun {
    /// Its name.
    #[serde(rename = "name")]
    pub name: String,

    /// When it was created — so one pending for hours reads as stuck.
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// A rule run's scope, summarised: a View by name, or the stored filter verbatim — a nested `or`
/// or a regex does not turn back into `search_catalog` filters.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub enum RunScope {
    /// A `view-ref`: the View's name.
    #[serde(rename = "view")]
    View(String),

    /// A `json-ast` or `raw-query`: the stored content.
    #[serde(rename = "filter")]
    Filter(Value),
}

/// One earlier finished run.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct HistoryEntry {
    /// Its name.
    #[serde(rename = "name")]
    pub name: String,

    /// `complete` or `error`.
    #[serde(rename = "status")]
    pub status: RunStatus,

    /// When it started.
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    /// When it ended.
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,

    /// Items in scope: `complete` rows only.
    #[serde(rename = "total", skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,

    /// Items that passed: `complete` rows only.
    #[serde(rename = "passed", skip_serializing_if = "Option::is_none")]
    pub passed: Option<u64>,

    /// Items that failed: `complete` rows only.
    #[serde(rename = "failed", skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,

    /// Items no rule applied to, when any.
    #[serde(rename = "skipped", skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,

    /// A rule's items outside its target types, when any.
    #[serde(rename = "outOfTarget", skip_serializing_if = "Option::is_none")]
    pub out_of_target: Option<u64>,

    /// Whether the rule or scorecard was updated after this run started.
    #[serde(rename = "updatedSinceRun", skip_serializing_if = "std::ops::Not::not")]
    pub updated_since_run: bool,
}

/// `get_compliance_results` — read the results of a compliance rule, scorecard or campaign
/// without running anything.
///
/// The reading half of `run_compliance_evaluation`, from the same shaping code: the latest
/// finished run, a short history with counts, any older run by name. Read-only, so a question
/// about the current state never starts a run.
pub struct GetComplianceResults {
    failure_sample: u16,
}

impl GetComplianceResults {
    /// The tool, with the defaults the configuration sets.
    pub fn new(defaults: &ToolDefaults) -> Self {
        Self {
            failure_sample: defaults.failure_sample,
        }
    }
}

impl Tool for GetComplianceResults {
    type Input = GetComplianceResultsInput;

    /// `readOnlyHint: true`; every other hint is the specification's default.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<GetComplianceResultsInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().read_only(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        validate(&input)?;

        let (sample, sample_echo) =
            effective_failure_sample(input.failure_sample, self.failure_sample);
        let reader = Reader {
            context,
            target: input.target,
            name: &input.name,
            sample,
        };

        let (output, warnings) = match input.target {
            Target::Campaign => reader.campaign().await?,
            Target::Rule => {
                let rule =
                    runs::read_target::<RuleSpec>(context.engine(), Target::Rule, &input.name)
                        .await?;
                reader
                    .runs(Definition::Rule(&rule), input.run.as_deref(), input.history)
                    .await?
            }
            Target::Scorecard => {
                let scorecard = runs::read_target::<ScorecardSpec>(
                    context.engine(),
                    Target::Scorecard,
                    &input.name,
                )
                .await?;
                reader
                    .runs(
                        Definition::Scorecard(&scorecard),
                        input.run.as_deref(),
                        input.history,
                    )
                    .await?
            }
        };

        let output = GetComplianceResultsOutput {
            result: output.result.with_failure_sample_echo(sample_echo),
            ..output
        };

        render(&output, warnings)
    }
}

/// Every input bound, checked before anything reaches the engine.
fn validate(input: &GetComplianceResultsInput) -> Result<(), ToolError> {
    for (field, value) in [("name", Some(&input.name)), ("run", input.run.as_ref())] {
        if let Some(value) = value
            && !is_valid_name(value)
        {
            return Err(filters::invalid(
                field,
                format!(
                    "`{value}` is not a valid name: lowercase letters, digits, `-` and `.`, at \
                     most 253 characters."
                ),
            ));
        }
    }

    if input.target == Target::Campaign && (input.run.is_some() || input.history.is_some()) {
        return Err(filters::invalid(
            if input.run.is_some() {
                "run"
            } else {
                "history"
            },
            "A campaign keeps only its latest result: it has no earlier runs to list or name."
                .to_string(),
        ));
    }

    if input.run.is_some() && input.history.is_some() {
        return Err(filters::invalid(
            "history",
            "`history` lists the runs before the latest; give `run` or `history`, not both — a \
             larger `history` reaches further back."
                .to_string(),
        ));
    }

    Ok(())
}

/// The definition a rule's or scorecard's results are read against.
enum Definition<'a> {
    Rule(&'a Read<RuleSpec>),
    Scorecard(&'a Read<ScorecardSpec>),
}

impl Definition<'_> {
    /// When the rule or scorecard was last updated, in any way.
    fn updated_at(&self) -> Option<&str> {
        match self {
            Self::Rule(read) => read.metadata.update_timestamp.as_deref(),
            Self::Scorecard(read) => read.metadata.update_timestamp.as_deref(),
        }
    }

    /// Whether it was updated strictly after `started_at`: a save and an evaluation in the same
    /// second — the website saves, then evaluates — do not count.
    fn updated_since(&self, started_at: Option<&str>) -> bool {
        updated_after(self.updated_at(), started_at)
    }

    /// Judges one run item against the definition.
    fn judge(
        &self,
        name: &str,
        item: Item,
        sample: u16,
    ) -> Result<(Outcome, Option<RunScope>), ToolError> {
        match self {
            Self::Rule(rule) => {
                let run: Read<RuleRunSpec> = runs::typed(item)?;
                let scope = run.spec.scope.as_ref().and_then(summarise_scope);
                Ok((judge_rule_run(name, run.spec, &rule.spec, sample), scope))
            }
            Self::Scorecard(scorecard) => {
                let evaluation: Read<ScorecardEvaluationSpec> = runs::typed(item)?;
                Ok((
                    judge_scorecard_evaluation(name, evaluation.spec, &scorecard.spec, sample),
                    None,
                ))
            }
        }
    }

    /// The URN a run of this target refers to it by.
    fn reference_field(&self) -> &'static str {
        match self {
            Self::Rule(_) => "ruleRef",
            Self::Scorecard(_) => "scorecardRef",
        }
    }
}

/// One read of one target's results.
struct Reader<'a> {
    context: &'a CallContext,
    target: Target,
    name: &'a str,
    sample: u16,
}

impl Reader<'_> {
    /// A rule's or scorecard's results: a named run, or the latest finished one with history.
    async fn runs(
        &self,
        definition: Definition<'_>,
        run: Option<&str>,
        history: Option<u8>,
    ) -> Result<(GetComplianceResultsOutput, Vec<String>), ToolError> {
        match run {
            Some(run) => self.named(&definition, run).await,
            None => self.latest(&definition, history).await,
        }
    }

    /// One run, by name, checked to belong to the target.
    async fn named(
        &self,
        definition: &Definition<'_>,
        run: &str,
    ) -> Result<(GetComplianceResultsOutput, Vec<String>), ToolError> {
        let family = self.target.run_family().unwrap_or_default();
        let address = crate::tools::compliance::compliance_address(family, run)?;
        let item = match self.context.engine().get_item(&address).await {
            Ok(response) => response.value,
            Err(error) if error.code == codes::NOT_FOUND => {
                return Err(ToolError::new(
                    codes::NOT_FOUND,
                    Remedy::RetryAfterChange,
                    format!(
                        "No {} named `{run}`; `history` lists the runs of `{}`.",
                        self.target.run_kind(),
                        self.name
                    ),
                ));
            }
            Err(error) => return Err(error),
        };

        let reference = item
            .spec
            .get(definition.reference_field())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if reference != self.target.urn(self.name) {
            let owner = parse_item_urn(&reference).map_or(reference.clone(), |parsed| parsed.name);
            return Err(filters::invalid(
                "run",
                format!("`{run}` is a run of `{owner}`, not of `{}`.", self.name),
            ));
        }

        let (outcome, scope) = definition.judge(self.name, item, self.sample)?;
        let (result, warnings, started) = self.finish(outcome, run);
        let updated_since_run = definition.updated_since(started.as_deref());

        Ok((
            GetComplianceResultsOutput {
                scope,
                updated_since_run,
                ..output(result)
            },
            warnings,
        ))
    }

    /// The latest finished run, a newer pending one if any, and the history before it.
    async fn latest(
        &self,
        definition: &Definition<'_>,
        history: Option<u8>,
    ) -> Result<(GetComplianceResultsOutput, Vec<String>), ToolError> {
        let wanted = history.unwrap_or(DEFAULT_HISTORY);
        let effective = wanted.min(MAX_HISTORY);
        let history_limit = (effective != wanted).then_some(effective);
        let engine = self.context.engine();

        let mut finished = runs::list_runs(
            engine,
            self.target,
            self.name,
            RunCondition::Finished,
            u32::from(effective) + 1,
        )
        .await?
        .into_iter();
        let pending =
            runs::newest_run(engine, self.target, self.name, RunCondition::Pending).await?;

        let Some(reported) = finished.next() else {
            return Ok(match pending {
                Some(pending) => {
                    let name = pending.metadata.name.clone();
                    let started = pending.metadata.creation_timestamp.clone();
                    let (result, warnings, _) = self.finish(Outcome::Pending(started), &name);
                    (output(result), warnings)
                }
                None => (
                    output(RunResult::new(RunStatus::None, self.target, self.name)),
                    vec![NEVER_EVALUATED_WARNING.to_string()],
                ),
            });
        };

        let reported_name = reported.metadata.name.clone();
        let reported_created = reported.metadata.creation_timestamp.clone();
        let (outcome, scope) = definition.judge(self.name, reported, self.sample)?;
        let (result, warnings, started) = self.finish(outcome, &reported_name);

        let newer_run = pending
            .filter(|pending| {
                is_later(
                    pending.metadata.creation_timestamp.as_deref(),
                    reported_created.as_deref(),
                )
            })
            .map(|pending| NewerRun {
                name: pending.metadata.name,
                started_at: pending.metadata.creation_timestamp,
            });

        let rows = finished
            .map(|item| self.history_entry(definition, item))
            .collect::<Result<Vec<_>, _>>()?;

        Ok((
            GetComplianceResultsOutput {
                scope,
                updated_since_run: definition.updated_since(started.as_deref()),
                newer_run,
                history: (!rows.is_empty()).then_some(rows),
                history_limit,
                ..output(result)
            },
            warnings,
        ))
    }

    /// One history row: the run's state and times, and counts when it completed.
    fn history_entry(
        &self,
        definition: &Definition<'_>,
        item: Item,
    ) -> Result<HistoryEntry, ToolError> {
        let name = item.metadata.name.clone();
        let (outcome, _) = definition.judge(self.name, item, 0)?;
        let result = match outcome {
            Outcome::Finished(result) => *result,
            // A finished listing returns finished runs; a run that moved meanwhile is shown as is.
            Outcome::Pending(started) => RunResult::new(RunStatus::Running, self.target, self.name)
                .with_times(started.as_deref(), None),
        };
        let non_zero = |count: Option<u64>| count.filter(|count| *count > 0);

        Ok(HistoryEntry {
            updated_since_run: definition.updated_since(result.started_at.as_deref()),
            name,
            status: result.status,
            started_at: result.started_at,
            ended_at: result.ended_at,
            total: result.total,
            passed: result.passed,
            failed: result.failed,
            skipped: non_zero(result.skipped),
            out_of_target: non_zero(result.out_of_target),
        })
    }

    /// A campaign's result, as the engine stores it: the latest evaluation decides; earlier
    /// results are shown only while a newer evaluation is pending.
    async fn campaign(&self) -> Result<(GetComplianceResultsOutput, Vec<String>), ToolError> {
        let campaign =
            runs::read_target::<CampaignSpec>(self.context.engine(), Target::Campaign, self.name)
                .await?;
        let results = campaign.spec.results.as_deref();
        let in_campaign =
            |result: RunResult| result.with_results_in(Target::Campaign.kind(), self.name);

        Ok(match campaign.spec.last_evaluation_result {
            None => (
                output(RunResult::new(RunStatus::None, self.target, self.name)),
                vec![NEVER_EVALUATED_WARNING.to_string()],
            ),
            Some(CampaignEvaluation::Complete { data }) => {
                let result = in_campaign(
                    RunResult::complete(
                        self.target,
                        self.name,
                        count_rules_items(results.unwrap_or_default(), None, self.sample),
                    )
                    .with_times(Some(&data.start_time), Some(&data.end_time)),
                );
                let warnings = zero_scope(&result);
                (output(result), warnings)
            }
            Some(CampaignEvaluation::Error { data }) => {
                let result = in_campaign(
                    RunResult::error(self.target, self.name, &data.error)
                        .with_times(Some(&data.start_time), Some(&data.end_time)),
                );
                let warnings = results
                    .is_some()
                    .then(|| EARLIER_RESULTS_NOT_SHOWN_WARNING.to_string())
                    .into_iter()
                    .collect();
                (output(result), warnings)
            }
            Some(CampaignEvaluation::Pending { data }) => match results {
                Some(results) => {
                    let earlier = count_rules_items(results, None, self.sample);
                    let result = RunResult {
                        status: RunStatus::Running,
                        ..RunResult::complete(self.target, self.name, earlier)
                    }
                    .with_times(Some(&data.start_time), None);
                    (
                        GetComplianceResultsOutput {
                            previous_results: true,
                            ..output(in_campaign(result))
                        },
                        vec![
                            running_warning(self.target, self.name, None),
                            PREVIOUS_RESULTS_WARNING.to_string(),
                        ],
                    )
                }
                None => (
                    output(in_campaign(
                        RunResult::new(RunStatus::Running, self.target, self.name)
                            .with_times(Some(&data.start_time), None),
                    )),
                    vec![running_warning(self.target, self.name, None)],
                ),
            },
        })
    }

    /// A judged run as a result: where it is stored, and the warnings it carries.
    fn finish(&self, outcome: Outcome, run: &str) -> (RunResult, Vec<String>, Option<String>) {
        match outcome {
            Outcome::Finished(result) => {
                let result = (*result).with_results_in(self.target.run_kind(), run);
                let warnings = zero_scope(&result);
                let started = result.started_at.clone();
                (result, warnings, started)
            }
            Outcome::Pending(started) => (
                RunResult::new(RunStatus::Running, self.target, self.name)
                    .with_times(started.as_deref(), None)
                    .with_results_in(self.target.run_kind(), run),
                vec![running_warning(self.target, self.name, Some(run))],
                started,
            ),
        }
    }
}

/// An output carrying only `result`.
fn output(result: RunResult) -> GetComplianceResultsOutput {
    GetComplianceResultsOutput {
        result,
        previous_results: false,
        scope: None,
        updated_since_run: false,
        newer_run: None,
        history: None,
        history_limit: None,
    }
}

/// The zero-scope warning, for a complete result over nothing.
fn zero_scope(result: &RunResult) -> Vec<String> {
    (result.status == RunStatus::Complete && result.total == Some(0))
        .then(|| ZERO_SCOPE_WARNING.to_string())
        .into_iter()
        .collect()
}

/// Whether `updated` is strictly later than `started`, both RFC 3339. Unknown is not later, and
/// the same second is not later: the website saves a scorecard and evaluates it in one go.
fn updated_after(updated: Option<&str>, started: Option<&str>) -> bool {
    match (updated.and_then(parse_time), started.and_then(parse_time)) {
        (Some(updated), Some(started)) => updated > started,
        _ => false,
    }
}

/// Whether `candidate` is strictly later than `than`, both RFC 3339; unknown is not later.
fn is_later(candidate: Option<&str>, than: Option<&str>) -> bool {
    match (candidate.and_then(parse_time), than.and_then(parse_time)) {
        (Some(candidate), Some(than)) => candidate > than,
        (Some(_), None) => true,
        _ => false,
    }
}

/// A stored scope, summarised: `view-ref` → the View's name; `json-ast` and `raw-query` → the
/// content verbatim.
fn summarise_scope(scope: &Value) -> Option<RunScope> {
    let content = scope.get("content")?;

    match scope.get("type").and_then(Value::as_str)? {
        "view-ref" => {
            let urn = content.as_str()?;
            Some(RunScope::View(
                parse_item_urn(urn)
                    .filter(|parsed| parsed.kind == VIEW_URN_KIND)
                    .map_or_else(|| urn.to_string(), |parsed| parsed.name),
            ))
        }
        "json-ast" | "raw-query" => Some(RunScope::Filter(content.clone())),
        _ => None,
    }
}

/// Renders the answer, with its warnings.
fn render(
    output: &GetComplianceResultsOutput,
    warnings: Vec<String>,
) -> Result<ToolOutput, ToolError> {
    let payload = serde_json::to_value(output).map_err(|err| {
        ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            format!("The compliance result could not be rendered: {err}"),
        )
    })?;

    Ok(warnings
        .into_iter()
        .fold(ToolOutput::new(payload), ToolOutput::with_warning))
}

#[cfg(test)]
mod tests;
