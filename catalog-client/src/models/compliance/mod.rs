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
// The `spec`s of the compliance items an evaluation writes or reads, as the engine stores them:
// a RuleRun, a ScorecardEvaluation, a Campaign's results, and the definitions the results are read
// against — a Rule's target types and a Scorecard's levels. Only the fields read are declared;
// everything else in a `spec` is ignored, so an engine adding a field breaks nothing.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// One item's outcome for one rule: `true`, `false`, or the string `"skipped"` — which a
/// ScorecardEvaluation stores, and a RuleRun's type definition allows although the engine writes
/// only booleans there today.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RuleOutcome {
    /// Passed (`true`) or failed (`false`).
    Bool(bool),

    /// Any string; `"skipped"` is the only one the engine writes.
    Text(String),
}

impl RuleOutcome {
    /// The string the engine writes for a rule that did not apply.
    pub const SKIPPED: &'static str = "skipped";

    /// Whether the rule passed.
    pub fn passed(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    /// Whether the rule failed.
    pub fn failed(&self) -> bool {
        matches!(self, Self::Bool(false))
    }

    /// Whether the rule did not apply.
    pub fn skipped(&self) -> bool {
        matches!(self, Self::Text(text) if text == Self::SKIPPED)
    }
}

/// A RuleRun's `spec`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct RuleRunSpec {
    /// The Rule's URN.
    #[serde(rename = "ruleRef")]
    pub rule_ref: String,

    /// When the run started.
    #[serde(rename = "runStartTime", default)]
    pub run_start_time: Option<String>,

    /// When it ended — set on `complete` and `error` alike.
    #[serde(rename = "runEndTime", default)]
    pub run_end_time: Option<String>,

    /// The scope the run was given, as stored: `view-ref`, `json-ast` or `raw-query`.
    #[serde(rename = "scope", default)]
    pub scope: Option<Value>,

    /// The outcome.
    #[serde(rename = "results")]
    pub results: RuleRunResults,
}

/// A RuleRun's `results`, tagged by `status`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RuleRunResults {
    /// Still running; `data` is `null`.
    Pending {},

    /// The evaluation failed.
    Error {
        /// Why.
        #[serde(rename = "data")]
        data: ErrorData,
    },

    /// Finished: one entry per item in scope.
    Complete {
        /// The per-item outcomes.
        #[serde(rename = "data")]
        data: Vec<RuleRunItem>,
    },
}

/// Why an evaluation failed.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct ErrorData {
    /// The engine's message.
    #[serde(rename = "error")]
    pub error: String,
}

/// One item's outcome in a RuleRun.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct RuleRunItem {
    /// The item's URN.
    #[serde(rename = "itemRef")]
    pub item_ref: String,

    /// The outcome.
    #[serde(rename = "result")]
    pub result: RuleOutcome,
}

/// A ScorecardEvaluation's `spec`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct ScorecardEvaluationSpec {
    /// The Scorecard's URN.
    #[serde(rename = "scorecardRef")]
    pub scorecard_ref: String,

    /// The outcome.
    #[serde(rename = "evaluation")]
    pub evaluation: Evaluation,
}

/// A ScorecardEvaluation's outcome, tagged by `status`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Evaluation {
    /// Still running.
    Pending {
        /// When it started.
        #[serde(rename = "data")]
        data: StartedData,
    },

    /// The evaluation failed.
    Error {
        /// When, and why.
        #[serde(rename = "data")]
        data: EndedWithError,
    },

    /// Finished.
    Complete {
        /// When, and one entry per item in scope.
        #[serde(rename = "data")]
        data: CompletedWithResults,
    },
}

/// A pending evaluation's data.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct StartedData {
    /// When it started.
    #[serde(rename = "startTime")]
    pub start_time: String,
}

/// A failed evaluation's data.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct EndedWithError {
    /// When it started.
    #[serde(rename = "startTime")]
    pub start_time: String,

    /// When it ended.
    #[serde(rename = "endTime")]
    pub end_time: String,

    /// The engine's message.
    #[serde(rename = "error")]
    pub error: String,
}

/// A completed scorecard evaluation's data.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct CompletedWithResults {
    /// When it started.
    #[serde(rename = "startTime")]
    pub start_time: String,

    /// When it ended.
    #[serde(rename = "endTime")]
    pub end_time: String,

    /// One entry per item in scope.
    #[serde(rename = "results", default)]
    pub results: Vec<RulesItem>,
}

/// One item's outcome for each rule of a scorecard or campaign.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct RulesItem {
    /// The item's URN.
    #[serde(rename = "itemRef")]
    pub item_ref: String,

    /// Each rule's outcome, by rule name.
    #[serde(rename = "rules")]
    pub rules: BTreeMap<String, RuleOutcome>,
}

/// The parts of a Campaign's `spec` an evaluation reads.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct CampaignSpec {
    /// When the campaign starts, RFC 3339.
    #[serde(rename = "startTime")]
    pub start_time: String,

    /// When it ends, RFC 3339.
    #[serde(rename = "endTime")]
    pub end_time: String,

    /// The latest evaluation's state. Absent until the first one.
    #[serde(rename = "lastEvaluationResult", default)]
    pub last_evaluation_result: Option<CampaignEvaluation>,

    /// The last **completed** evaluation's results — kept while a later one is pending or failed.
    #[serde(rename = "results", default)]
    pub results: Option<Vec<RulesItem>>,
}

/// A campaign's latest evaluation, tagged by `status`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CampaignEvaluation {
    /// Started and not finished.
    Pending {
        /// When it started.
        #[serde(rename = "data")]
        data: StartedData,
    },

    /// The evaluation failed.
    Error {
        /// When, and why.
        #[serde(rename = "data")]
        data: EndedWithError,
    },

    /// Finished; the results are the campaign's `results`.
    Complete {
        /// When.
        #[serde(rename = "data")]
        data: Ended,
    },
}

impl CampaignEvaluation {
    /// When the evaluation started.
    pub fn start_time(&self) -> &str {
        match self {
            Self::Pending { data } => &data.start_time,
            Self::Error { data } => &data.start_time,
            Self::Complete { data } => &data.start_time,
        }
    }
}

/// A finished evaluation's times.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct Ended {
    /// When it started.
    #[serde(rename = "startTime")]
    pub start_time: String,

    /// When it ended.
    #[serde(rename = "endTime")]
    pub end_time: String,
}

/// The part of a Rule's `spec` a run's results are read against.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct RuleSpec {
    /// The item types the rule is meant for. Absent: every type.
    #[serde(rename = "targetItemTypes", default)]
    pub target_item_types: Option<TargetItemTypes>,
}

/// Which item types a rule applies to.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct TargetItemTypes {
    /// `include` keeps the listed types; `exclude` keeps every other.
    #[serde(rename = "mode")]
    pub mode: TargetMode,

    /// Item type URNs: `urn:mia-platform-catalog:<group>:<version>:<kind>`.
    #[serde(rename = "itemTypeRefs")]
    pub item_type_refs: Vec<String>,
}

/// How a rule's `itemTypeRefs` are applied.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetMode {
    /// Only the listed types.
    Include,

    /// Every type but the listed ones.
    Exclude,
}

/// The part of a Scorecard's `spec` its results are read against.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct ScorecardSpec {
    /// The levels, **highest first**. Absent or empty: the scorecard has none.
    #[serde(rename = "levels", default)]
    pub levels: Vec<ScorecardLevel>,

    /// The rules, inline or by reference, each optionally in a level.
    #[serde(rename = "rules", default)]
    pub rules: Vec<ScorecardRule>,
}

/// One level of a scorecard.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct ScorecardLevel {
    /// Its name, which rules refer to.
    #[serde(rename = "name")]
    pub name: String,
}

/// One rule of a scorecard, tagged by `type`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
#[serde(tag = "type")]
pub enum ScorecardRule {
    /// Written in the scorecard.
    #[serde(rename = "inline")]
    Inline {
        /// The rule.
        #[serde(rename = "content")]
        content: InlineRule,

        /// The level it belongs to, if any.
        #[serde(rename = "level", default)]
        level: Option<String>,
    },

    /// A Rule item, by URN.
    #[serde(rename = "rule-ref")]
    RuleRef {
        /// The Rule's URN.
        #[serde(rename = "content")]
        content: String,

        /// The level it belongs to, if any.
        #[serde(rename = "level", default)]
        level: Option<String>,
    },
}

/// An inline rule's name, which is the key of its results.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(PartialEq))]
pub struct InlineRule {
    /// The rule's name.
    #[serde(rename = "name")]
    pub name: String,
}

impl ScorecardRule {
    /// The key the rule's results are stored under: an inline rule's own name, or the last
    /// segment of a referenced Rule's URN, which is that Rule's `metadata.name`.
    pub fn result_key(&self) -> &str {
        match self {
            Self::Inline { content, .. } => &content.name,
            Self::RuleRef { content, .. } => content.rsplit(':').next().unwrap_or(content),
        }
    }

    /// The level the rule belongs to, if any.
    pub fn level(&self) -> Option<&str> {
        match self {
            Self::Inline { level, .. } | Self::RuleRef { level, .. } => level.as_deref(),
        }
    }
}

#[cfg(test)]
mod tests;
