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
            NOT_YET_STARTED_WARNING, Target, ZERO_SCOPE_WARNING, parse_item_urn, parse_time,
            running_warning,
            runs::{self, Read, RunCondition},
            shape::{
                Outcome, RunResult, RunStatus, count_rules_items, effective_failure_sample,
                judge_rule_run, judge_scorecard_evaluation,
            },
        },
        filters::{self, FieldFilters, SPEC_WITHOUT_KIND_WARNING},
    },
};
use catalog_client::{
    Remedy, ToolError,
    error::{BadRequestOrigin, codes},
    is_valid_name,
    models::compliance::{
        CampaignEvaluation, CampaignSpec, RuleRunSpec, RuleSpec, ScorecardEvaluationSpec,
        ScorecardSpec,
    },
};
use chrono::{DateTime, SubsecRound, Utc};
use configuration::tools::ToolDefaults;
use rmcp::model::ToolAnnotations;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// A rule's scope, as the engine takes it.
mod scope;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "run_compliance_evaluation";

/// What the tool does. Names every argument and the outputs that decide the next step, the
/// asynchrony above all: it is the one behaviour a model cannot infer, and getting it wrong means
/// reporting "compliant" for a run that has not finished.
const TOOL_DESCRIPTION: &str = "Runs a compliance rule, scorecard or campaign (`target`, `name`) \
     and reports the result: `status` (complete, running, error), `total`, `passed`, `failed`, \
     `skipped`, the `failures` (up to `failure_sample`, default 50; `sampled` when cut — counts \
     stay exact), `failedByKind`, and for scorecards `byRule`, `byLevel` and `minimumLevel`. A \
     rule needs a `scope`: a `view`, or search_catalog's filters; search_catalog with the same \
     filters gives its size as `total`. A rule's items outside its target types are \
     `outOfTarget`, not failed. It waits up to `wait` seconds (default 20); if still running, read \
     it later with get_compliance_results using `resultsIn`. `failed` includes items the rule \
     could not be evaluated on, and `total` 0 proves nothing. Running a campaign replaces its \
     stored results.";

/// What the wait leaves for the final read and the shaping, so a wait never ends in the deadline.
const RESERVE: Duration = Duration::from_secs(3);

/// The poll's intervals: quick at first, since a run over nothing finishes at once, then half
/// the engine's own five-second tick, so a written result is seen within that.
const INTERVALS: [Duration; 3] = [
    Duration::from_millis(1_000),
    Duration::from_millis(2_000),
    Duration::from_millis(2_500),
];

/// What a progress notification says while the tool waits.
const PROGRESS_MESSAGE: &str = "waiting for the evaluation";

/// Arguments for `run_compliance_evaluation`. Conventions as `SearchCatalogInput`: unknown fields
/// refused, no `#[serde(default)]`.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct RunComplianceEvaluationInput {
    /// What to run: rule, scorecard or campaign.
    #[serde(rename = "target")]
    pub target: Target,

    /// Its name.
    #[serde(rename = "name")]
    pub name: String,

    /// Rule only: the items to evaluate.
    #[serde(rename = "scope")]
    pub scope: Option<ComplianceScope>,

    /// Seconds to wait for the result. Default 20; 0 returns at once.
    #[serde(rename = "wait")]
    pub wait: Option<u16>,

    /// Failures to list. Default 50, clamped to 500; counts stay exact.
    #[serde(rename = "failure_sample")]
    pub failure_sample: Option<u16>,
}

/// A View, or search_catalog's filters.
///
/// Unknown fields are refused here above all: a mistyped `label` beside a `kind` would otherwise
/// widen the scope to every item of that kind — an evaluation over the wrong set, reported as
/// authoritative.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct ComplianceScope {
    /// A View, by name or URN. Alone.
    #[serde(rename = "view")]
    pub view: Option<String>,

    /// As search_catalog.
    #[serde(rename = "kind")]
    pub kind: Option<String>,

    /// As search_catalog.
    #[serde(rename = "group")]
    pub group: Option<String>,

    /// As search_catalog.
    #[serde(rename = "labels")]
    pub labels: Option<BTreeMap<String, String>>,

    /// As search_catalog.
    #[serde(rename = "fields")]
    pub fields: Option<FieldFilters>,

    /// As search_catalog.
    #[serde(rename = "query")]
    pub query: Option<String>,
}

/// `run_compliance_evaluation` — start a compliance evaluation, wait for it in the cluster, and
/// report what failed.
///
/// The engine only *starts* an evaluation: a rule answers with a new RuleRun's URN, a scorecard
/// and a campaign with their own. This tool absorbs that — it locates where the result will land,
/// polls it at the engine's pace, and shapes it — so the model gets one answer in one round trip,
/// or a precise hand-back when the run outlasts the wait.
pub struct RunComplianceEvaluation {
    wait_seconds: u64,
    failure_sample: u16,
    intervals: &'static [Duration],
}

impl RunComplianceEvaluation {
    /// The tool, with the defaults the configuration sets.
    pub fn new(defaults: &ToolDefaults) -> Self {
        Self {
            wait_seconds: defaults.compliance_wait_seconds,
            failure_sample: defaults.failure_sample,
            intervals: &INTERVALS,
        }
    }

    /// The tool polling at `intervals`, so a test waits milliseconds rather than seconds.
    #[cfg(test)]
    pub fn with_intervals(defaults: &ToolDefaults, intervals: &'static [Duration]) -> Self {
        Self {
            intervals,
            ..Self::new(defaults)
        }
    }
}

impl Tool for RunComplianceEvaluation {
    type Input = RunComplianceEvaluationInput;

    /// Not read-only — it creates a run, or rewrites a campaign's results — and not idempotent:
    /// each call starts a new run.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<RunComplianceEvaluationInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().destructive(false).idempotent(false),
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
        let requested_wait = Duration::from_secs(input.wait.map_or(self.wait_seconds, u64::from));
        let engine = context.engine();
        let target = input.target;
        let name = input.name.as_str();
        let mut warnings = Vec::new();

        // Pre-flight: what the result will be read against, and the state the engine would refuse.
        let preflight = match target {
            Target::Rule => {
                let rule = runs::read_target::<RuleSpec>(engine, target, name).await?;
                let scope = input.scope.as_ref().ok_or_else(missing_scope)?;
                let built = scope::build(engine, scope).await?;
                if built.spec_without_kind {
                    warnings.push(SPEC_WITHOUT_KIND_WARNING.to_string());
                }
                Preflight::Rule {
                    rule: rule.spec,
                    body: built.body,
                }
            }
            Target::Scorecard => {
                let scorecard = runs::read_target::<ScorecardSpec>(engine, target, name).await?;
                if let Some(pending) =
                    runs::newest_run(engine, target, name, RunCondition::Pending).await?
                {
                    return Err(already_pending(
                        target,
                        name,
                        &pending.metadata.name,
                        pending.metadata.creation_timestamp.as_deref(),
                    ));
                }
                Preflight::Scorecard {
                    scorecard: scorecard.spec,
                }
            }
            Target::Campaign => {
                let campaign = runs::read_target::<CampaignSpec>(engine, target, name).await?;
                check_campaign(name, &campaign.spec, Utc::now())?;
                Preflight::Campaign {
                    before: campaign.spec.last_evaluation_result,
                }
            }
        };

        // A caller already gone is told so before anything starts: nothing ran, a retry is safe.
        if context.cancellation().is_cancelled() {
            return Err(catalog_client::error::cancelled());
        }

        // The POST: sent once, never retried — a retry would start a second run.
        let posted_at = Utc::now().trunc_subsecs(0);
        let (body, origin) = match &preflight {
            Preflight::Rule { body, .. } => (Some(body), BadRequestOrigin::ServerBuilt),
            _ => (None, BadRequestOrigin::CallerInput),
        };
        let started = engine
            .evaluate(target.evaluation_family(), name, body, origin)
            .await
            .map_err(|error| refused_by_state(target, error))?
            .value;

        // From here on nothing may tell the model to retry.
        let wait = requested_wait.min(context.deadline().remaining().saturating_sub(RESERVE));
        let poll = Poll {
            context,
            target,
            name,
            sample,
            wait,
            posted_at,
            intervals: self.intervals,
        };

        let (result, mut poll_warnings) = poll.run(&preflight, &started.urn).await?;
        warnings.append(&mut poll_warnings);

        render(result.with_failure_sample_echo(sample_echo), warnings)
    }
}

/// What the pre-flight read, and what the result is judged against.
enum Preflight {
    Rule {
        rule: RuleSpec,
        body: serde_json::Value,
    },
    Scorecard {
        scorecard: ScorecardSpec,
    },
    Campaign {
        before: Option<CampaignEvaluation>,
    },
}

/// Every input bound, checked before anything reaches the engine.
fn validate(input: &RunComplianceEvaluationInput) -> Result<(), ToolError> {
    if !is_valid_name(&input.name) {
        return Err(filters::invalid(
            "name",
            format!(
                "`{}` is not a valid name: lowercase letters, digits, `-` and `.`, at most 253 \
                 characters.",
                input.name
            ),
        ));
    }

    match (input.target, &input.scope) {
        (Target::Rule, None) => Err(missing_scope()),
        (Target::Rule, Some(scope)) => scope::validate(scope),
        (_, Some(_)) => Err(filters::invalid(
            "scope",
            "Only a rule takes a `scope`: a scorecard or a campaign is evaluated over the scope \
             it holds, and a scope sent with it would be ignored."
                .to_string(),
        )),
        (_, None) => Ok(()),
    }
}

/// A rule without a scope.
fn missing_scope() -> ToolError {
    filters::invalid(
        "scope",
        "A rule needs a `scope`: a `view`, or search_catalog's filters (`kind`, `group`, `labels`, \
         `fields`, `query`)."
            .to_string(),
    )
}

/// An evaluation already pending, which the engine would refuse.
fn already_pending(target: Target, name: &str, pending: &str, since: Option<&str>) -> ToolError {
    ToolError::new(
        codes::CONFLICT,
        Remedy::RetryLater,
        format!(
            "An evaluation of the {} `{name}` is already running (`{pending}`, started {}); the \
             catalog runs one at a time. A run pending for a long time may be stuck.",
            target.kind().to_lowercase(),
            since.unwrap_or("at an unknown time")
        ),
    )
}

/// The campaign states the engine would refuse: pending, not started, expired.
fn check_campaign(
    name: &str,
    campaign: &CampaignSpec,
    now: DateTime<Utc>,
) -> Result<(), ToolError> {
    if let Some(CampaignEvaluation::Pending { data }) = &campaign.last_evaluation_result {
        return Err(already_pending(
            Target::Campaign,
            name,
            name,
            Some(&data.start_time),
        ));
    }

    if let Some(start) = parse_time(&campaign.start_time)
        && start > now
    {
        return Err(ToolError::new(
            codes::CONFLICT,
            Remedy::RetryLater,
            format!(
                "The campaign `{name}` has not started yet: it starts at {}.",
                campaign.start_time
            ),
        ));
    }

    if let Some(end) = parse_time(&campaign.end_time)
        && end < now
    {
        return Err(ToolError::new(
            codes::CONFLICT,
            Remedy::Escalate,
            format!(
                "The campaign `{name}` ended at {} and can no longer be evaluated; only someone \
                 who can edit the campaign can extend it.",
                campaign.end_time
            ),
        ));
    }

    Ok(())
}

/// A scorecard or campaign refused after the pre-flight passed: its state changed in between.
///
/// The evaluate call attributes their `400` to the caller's subject, so it arrives as
/// `invalid_input`; it is a state, not an input — `conflict`, with the engine's message.
fn refused_by_state(target: Target, error: ToolError) -> ToolError {
    if target != Target::Rule && error.code == codes::INVALID_INPUT {
        ToolError::new(codes::CONFLICT, Remedy::RetryLater, error.message)
    } else {
        error
    }
}

/// The progress point after `elapsed` of a `wait`: seconds waited, of the seconds allowed.
fn progress_point(elapsed: Duration, wait: Duration) -> (u32, u32) {
    let seconds = |duration: Duration| u32::try_from(duration.as_secs()).unwrap_or(u32::MAX);

    (seconds(elapsed).min(seconds(wait)), seconds(wait))
}

/// Renders the answer, with its warnings — a result over nothing always carrying its own.
fn render(result: RunResult, mut warnings: Vec<String>) -> Result<ToolOutput, ToolError> {
    if result.status == RunStatus::Complete && result.total == Some(0) {
        warnings.insert(0, ZERO_SCOPE_WARNING.to_string());
    }

    let payload = serde_json::to_value(&result).map_err(|err| {
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

/// Where the result lands, once the POST has succeeded.
enum Location {
    /// A RuleRun or a ScorecardEvaluation.
    Run { family: &'static str, name: String },

    /// A scorecard's evaluation that could not be located: `get_compliance_results` finds it.
    Unlocated,

    /// The campaign itself.
    Campaign,
}

/// One wait for one run.
struct Poll<'a> {
    context: &'a CallContext,
    target: Target,
    name: &'a str,
    sample: u16,
    wait: Duration,
    posted_at: DateTime<Utc>,
    intervals: &'a [Duration],
}

impl From<Outcome> for Judged {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Pending(started) => Self::Pending(started),
            Outcome::Finished(result) => Self::Done(result, Vec::new()),
        }
    }
}

/// What reading the result found.
enum Judged {
    /// Finished: the result to return.
    Done(Box<RunResult>, Vec<String>),

    /// Not finished; when it started, if known.
    Pending(Option<String>),
}

impl Poll<'_> {
    /// Locates the result, then waits for it.
    async fn run(
        &self,
        preflight: &Preflight,
        urn: &str,
    ) -> Result<(RunResult, Vec<String>), ToolError> {
        let location = self.locate(urn).await;

        if self.wait.is_zero() {
            return Ok(self.hand_back(&location, None));
        }

        let (family, item) = match &location {
            Location::Run { family, name } => (*family, name.as_str()),
            Location::Campaign => (self.target.family(), self.name),
            Location::Unlocated => return Ok(self.hand_back(&location, None)),
        };

        let clock = Instant::now();
        let mut baseline: Option<String> = None;
        let mut started_at: Option<String> = None;
        let mut iteration = 0;

        loop {
            let delay = interval(self.intervals, iteration);
            if clock.elapsed() + delay > self.wait {
                return Ok(self.hand_back(&location, started_at));
            }

            tokio::select! {
                () = self.context.cancellation().cancelled() => {
                    return Err(self.cancelled_after_post(&location));
                }
                () = tokio::time::sleep(delay) => {}
            }
            iteration += 1;

            if let Some(progress) = self.context.progress() {
                let (done, total) = progress_point(clock.elapsed(), self.wait);
                progress
                    .send(done, Some(total), Some(PROGRESS_MESSAGE))
                    .await;
            }

            // A full read only when the item changed — compared for equality, never order.
            if baseline.is_some() {
                match runs::read_version(self.context.engine(), family, item).await {
                    Ok(version) if version == baseline => continue,
                    Ok(_) => {}
                    Err(error) if error.code == codes::NOT_FOUND => return Err(deleted(item)),
                    Err(_) => return Ok(self.hand_back(&location, started_at)),
                }
            }

            let judged = match self.read_and_judge(preflight, family, item).await {
                Ok((judged, version)) => {
                    baseline = version;
                    judged
                }
                Err(error) if error.code == codes::NOT_FOUND => return Err(deleted(item)),
                Err(_) => return Ok(self.hand_back(&location, started_at)),
            };

            match judged {
                Judged::Done(result, warnings) => {
                    return Ok((
                        (*result).with_results_in(self.target.run_kind(), item),
                        warnings,
                    ));
                }
                Judged::Pending(started) => started_at = started.or(started_at),
            }
        }
    }

    /// Where the result lands: a rule's run is the returned URN; a scorecard's is its newest
    /// evaluation, which the engine creates before answering; a campaign's is the campaign.
    async fn locate(&self, urn: &str) -> Location {
        match self.target {
            Target::Rule => match parse_item_urn(urn) {
                Some(parsed) => Location::Run {
                    family: "rule-runs",
                    name: parsed.name,
                },
                None => Location::Unlocated,
            },
            Target::Scorecard => {
                match runs::newest_run(
                    self.context.engine(),
                    self.target,
                    self.name,
                    RunCondition::Any,
                )
                .await
                {
                    Ok(Some(newest)) => Location::Run {
                        family: "scorecard-evaluations",
                        name: newest.metadata.name,
                    },
                    _ => Location::Unlocated,
                }
            }
            Target::Campaign => Location::Campaign,
        }
    }

    /// Reads the result whole and judges it; returns the `resourceVersion` it was read at.
    async fn read_and_judge(
        &self,
        preflight: &Preflight,
        family: &str,
        item: &str,
    ) -> Result<(Judged, Option<String>), ToolError> {
        let engine = self.context.engine();

        match preflight {
            Preflight::Rule { rule, .. } => {
                let read: Read<RuleRunSpec> = runs::read(engine, family, item).await?;
                let judged = judge_rule_run(self.name, read.spec, rule, self.sample).into();
                Ok((judged, read.resource_version))
            }
            Preflight::Scorecard { scorecard } => {
                let read: Read<ScorecardEvaluationSpec> = runs::read(engine, family, item).await?;
                let judged =
                    judge_scorecard_evaluation(self.name, read.spec, scorecard, self.sample).into();
                Ok((judged, read.resource_version))
            }
            Preflight::Campaign { before } => {
                let read: Read<CampaignSpec> = runs::read(engine, family, item).await?;
                let judged = match read.spec.last_evaluation_result {
                    Some(latest) if self.is_new(before.as_ref(), &latest) => match latest {
                        CampaignEvaluation::Pending { data } => {
                            Judged::Pending(Some(data.start_time))
                        }
                        CampaignEvaluation::Error { data } => Judged::Done(
                            Box::new(
                                RunResult::error(self.target, self.name, &data.error)
                                    .with_times(Some(&data.start_time), Some(&data.end_time)),
                            ),
                            Vec::new(),
                        ),
                        CampaignEvaluation::Complete { data } => Judged::Done(
                            Box::new(
                                RunResult::complete(
                                    self.target,
                                    self.name,
                                    count_rules_items(
                                        read.spec.results.as_deref().unwrap_or_default(),
                                        None,
                                        self.sample,
                                    ),
                                )
                                .with_times(Some(&data.start_time), Some(&data.end_time)),
                            ),
                            Vec::new(),
                        ),
                    },
                    // Still the previous run's result, or none: the worker has not started it.
                    _ => Judged::Pending(None),
                };
                Ok((judged, read.resource_version))
            }
        }
    }

    /// Whether a campaign's latest evaluation is this call's.
    fn is_new(&self, before: Option<&CampaignEvaluation>, latest: &CampaignEvaluation) -> bool {
        is_this_calls_evaluation(before, latest, self.posted_at)
    }

    /// A `running` answer with what is known, and where to read the result later.
    fn hand_back(
        &self,
        location: &Location,
        started_at: Option<String>,
    ) -> (RunResult, Vec<String>) {
        let (kind, item, run) = match location {
            Location::Run { name, .. } => {
                (self.target.run_kind(), name.as_str(), Some(name.as_str()))
            }
            Location::Unlocated => (self.target.kind(), self.name, None),
            Location::Campaign => (self.target.kind(), self.name, None),
        };

        let mut warnings = vec![running_warning(self.target, self.name, run)];
        if matches!(location, Location::Campaign) && started_at.is_none() {
            warnings.push(NOT_YET_STARTED_WARNING.to_string());
        }

        (
            RunResult::new(RunStatus::Running, self.target, self.name)
                .with_times(started_at.as_deref(), None)
                .with_results_in(kind, item),
            warnings,
        )
    }

    /// The caller went away after the run started: a retry would start another.
    fn cancelled_after_post(&self, location: &Location) -> ToolError {
        let (result, _) = self.hand_back(location, None);
        let where_ = result
            .results_in
            .map(|results_in| format!("{} `{}`", results_in.kind, results_in.name))
            .unwrap_or_default();

        ToolError::new(
            codes::CANCELLED,
            Remedy::Unknown,
            format!(
                "The call was cancelled after the evaluation started; it is still running in \
                 {where_}. Read it with get_compliance_results rather than running it again."
            ),
        )
    }
}

/// The wait before poll number `iteration`: the schedule's steps, then its last step for good.
fn interval(schedule: &[Duration], iteration: usize) -> Duration {
    schedule
        .get(iteration)
        .or_else(|| schedule.last())
        .copied()
        .unwrap_or(INTERVALS[INTERVALS.len() - 1])
}

/// Whether a campaign's `latest` evaluation is the one this call started: different from the one
/// read before the POST, **and** started no earlier than the POST, floored to the second as the
/// engine stores it. The first test rejects a previous run started and finished in that same
/// second; the second, a result that changed for another reason.
fn is_this_calls_evaluation(
    before: Option<&CampaignEvaluation>,
    latest: &CampaignEvaluation,
    posted_at: DateTime<Utc>,
) -> bool {
    let differs = before.is_none_or(|before| fingerprint(before) != fingerprint(latest));
    let not_earlier = parse_time(latest.start_time()).is_some_and(|started| started >= posted_at);

    differs && not_earlier
}

/// What tells two campaign evaluations apart.
fn fingerprint(evaluation: &CampaignEvaluation) -> (&'static str, &str, Option<&str>) {
    match evaluation {
        CampaignEvaluation::Pending { data } => ("pending", &data.start_time, None),
        CampaignEvaluation::Error { data } => ("error", &data.start_time, Some(&data.end_time)),
        CampaignEvaluation::Complete { data } => {
            ("complete", &data.start_time, Some(&data.end_time))
        }
    }
}

/// The result item disappeared while the run was being waited on.
fn deleted(item: &str) -> ToolError {
    ToolError::new(
        codes::NOT_FOUND,
        Remedy::Escalate,
        format!(
            "`{item}`, where the result was being written, was deleted while the run was waited on."
        ),
    )
}

#[cfg(test)]
mod tests;
