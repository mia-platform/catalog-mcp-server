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
// What `run_compliance_evaluation` and `get_compliance_results` share: the targets, how their runs
// are found and read, how a run's result is shaped, and the fixed wording that goes with it. One
// place, so the answer about a run is the same whichever of the two tools gives it.

use catalog_client::{
    ItemAddress, ToolError,
    ops::compliance::{COMPLIANCE_GROUP, COMPLIANCE_VERSION, EvaluationFamily},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Finding and reading runs, and the definitions their results are read against.
pub mod runs;

/// A run's result, shaped: counts, failures, levels, out-of-target items.
pub mod shape;

/// The scheme every catalog URN starts with: `urn:mia-platform-catalog:<group>:<version>:<kind>…`.
const URN_PREFIX: &str = "urn:mia-platform-catalog:";

/// What is evaluated, or whose results are read.
///
/// A rule is evaluated over a scope given with the request; a scorecard and a campaign over the
/// scope they hold, and a campaign's result is stored on the campaign itself. The variants carry
/// one word each, because their doc comments reach every input schema that names a target.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// A rule.
    Rule,

    /// A scorecard.
    Scorecard,

    /// A campaign.
    Campaign,
}

impl Target {
    /// The family of the target's own items.
    pub fn family(self) -> &'static str {
        self.evaluation_family().as_str()
    }

    /// The family as the evaluate operation takes it.
    pub fn evaluation_family(self) -> EvaluationFamily {
        match self {
            Self::Rule => EvaluationFamily::Rules,
            Self::Scorecard => EvaluationFamily::Scorecards,
            Self::Campaign => EvaluationFamily::Campaigns,
        }
    }

    /// The target's own `kind`.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Rule => "Rule",
            Self::Scorecard => "Scorecard",
            Self::Campaign => "Campaign",
        }
    }

    /// The family a run of the target is stored in. A campaign has none: its result is stored on
    /// the campaign itself.
    pub fn run_family(self) -> Option<&'static str> {
        match self {
            Self::Rule => Some("rule-runs"),
            Self::Scorecard => Some("scorecard-evaluations"),
            Self::Campaign => None,
        }
    }

    /// The `kind` of a run of the target, or of the item holding its result.
    pub fn run_kind(self) -> &'static str {
        match self {
            Self::Rule => "RuleRun",
            Self::Scorecard => "ScorecardEvaluation",
            Self::Campaign => "Campaign",
        }
    }

    /// The field of a run that names its target: `spec.ruleRef` or `spec.scorecardRef`.
    pub fn run_reference_field(self) -> Option<&'static str> {
        match self {
            Self::Rule => Some("spec.ruleRef"),
            Self::Scorecard => Some("spec.scorecardRef"),
            Self::Campaign => None,
        }
    }

    /// The URN of the target named `name`, as a run refers to it.
    pub fn urn(self, name: &str) -> String {
        format!(
            "{URN_PREFIX}{COMPLIANCE_GROUP}:{COMPLIANCE_VERSION}:{}:{name}",
            self.kind()
        )
    }
}

/// The address of a compliance item: in the compliance group, at its served version.
pub fn compliance_address(family: &str, name: &str) -> Result<ItemAddress, ToolError> {
    ItemAddress::new(COMPLIANCE_GROUP, COMPLIANCE_VERSION, family, name)
}

/// An item, as its URN names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRef {
    /// The group half of its `apiVersion`.
    pub group: String,

    /// The version half of its `apiVersion`.
    pub version: String,

    /// Its `kind`.
    pub kind: String,

    /// Its `metadata.name`.
    pub name: String,
}

/// Parses `urn:mia-platform-catalog:<group>:<version>:<kind>:<name>`.
///
/// A catalog name cannot contain a colon, so the split is exact. Anything else is `None`.
pub fn parse_item_urn(urn: &str) -> Option<ItemRef> {
    let parts: [&str; 4] = urn
        .strip_prefix(URN_PREFIX)?
        .split(':')
        .collect::<Vec<_>>()
        .try_into()
        .ok()?;

    match parts {
        [group, version, kind, name]
            if !group.is_empty() && !version.is_empty() && !kind.is_empty() && !name.is_empty() =>
        {
            Some(ItemRef {
                group: group.to_string(),
                version: version.to_string(),
                kind: kind.to_string(),
                name: name.to_string(),
            })
        }
        _ => None,
    }
}

/// Parses an item type URN, `urn:mia-platform-catalog:<group>:<version>:<kind>`, into its
/// `apiVersion` and `kind` — the pair the policy engine matches target types on.
pub fn parse_type_urn(urn: &str) -> Option<(String, String)> {
    let parts: [&str; 3] = urn
        .strip_prefix(URN_PREFIX)?
        .split(':')
        .collect::<Vec<_>>()
        .try_into()
        .ok()?;

    match parts {
        [group, version, kind] if !group.is_empty() && !version.is_empty() && !kind.is_empty() => {
            Some((format!("{group}/{version}"), kind.to_string()))
        }
        _ => None,
    }
}

/// An RFC 3339 time, or `None` when the engine wrote something else.
pub fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// Said with a result over no items at all.
pub const ZERO_SCOPE_WARNING: &str = "Nothing was in scope, so this result proves nothing about \
     compliance: check the scope.";

/// Said with a campaign evaluation the engine accepted but has not started yet.
pub const NOT_YET_STARTED_WARNING: &str = "The campaign's evaluation was accepted but has not \
     started yet.";

/// Said with a campaign's earlier results, shown while a newer evaluation is pending.
pub const PREVIOUS_RESULTS_WARNING: &str = "A newer evaluation is still running; these are the \
     results of the last completed one, whose dates the campaign no longer keeps.";

/// Said with a campaign whose latest evaluation failed while it still holds earlier results.
pub const EARLIER_RESULTS_NOT_SHOWN_WARNING: &str = "The campaign still holds the results of an \
     earlier completed evaluation; they are not shown, because the latest one failed.";

/// Said with a target that has never been evaluated.
pub const NEVER_EVALUATED_WARNING: &str = "It has never been evaluated; run_compliance_evaluation \
     runs it.";

/// Said with a run still going: where to read it later, and that it may never finish.
pub fn running_warning(target: Target, name: &str, run: Option<&str>) -> String {
    let follow_up = match run {
        Some(run) => format!(
            "get_compliance_results with target `{}`, name `{name}` and run `{run}`",
            target_name(target)
        ),
        None => format!(
            "get_compliance_results with target `{}` and name `{name}`",
            target_name(target)
        ),
    };

    format!(
        "Still running, so this is not a result. Read it later with {follow_up}. A run pending for \
         a long time may be stuck: compare its startedAt with now."
    )
}

/// The wire name of a target, as the tools take it.
fn target_name(target: Target) -> &'static str {
    match target {
        Target::Rule => "rule",
        Target::Scorecard => "scorecard",
        Target::Campaign => "campaign",
    }
}

#[cfg(test)]
mod tests;
