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
    client::{EngineClient, EngineResponse},
    error::{BadRequestOrigin, ToolError},
    ops::EVALUATE_COMPLIANCE,
};
use serde::Deserialize;
use serde_json::Value;

/// The group every compliance type belongs to.
pub const COMPLIANCE_GROUP: &str = "compliance.mia-platform.eu";

/// The version every compliance type is served at.
pub const COMPLIANCE_VERSION: &str = "v1";

/// The path segment that turns an item's address into its evaluation.
const EVALUATE_SEGMENT: &str = "evaluate";

/// What can be evaluated: the three compliance families with an `evaluate` endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluationFamily {
    /// `rules` — evaluated over a scope given with the request.
    Rules,

    /// `scorecards` — evaluated over the scope the scorecard holds.
    Scorecards,

    /// `campaigns` — evaluated over the scope the campaign holds.
    Campaigns,
}

impl EvaluationFamily {
    /// The family's path segment.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rules => "rules",
            Self::Scorecards => "scorecards",
            Self::Campaigns => "campaigns",
        }
    }
}

/// What the engine answers an evaluation with: one URN, never the result.
///
/// For a rule it is the new RuleRun's; for a scorecard or a campaign, the scorecard's or the
/// campaign's own — the result lands later, elsewhere.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct EvaluationStarted {
    /// The URN the engine answered with.
    #[serde(rename = "urn")]
    pub urn: String,
}

impl EngineClient {
    /// `POST /compliance.mia-platform.eu/v1/items/{family}/{name}/evaluate` — start an evaluation.
    ///
    /// `body` is a rule's `{"scope": …}` and absent for the other two, which take none. **Sent
    /// once and never retried** (see [`EngineClient::post_json`]): a repeat would start a second
    /// run. `origin` says whose fault a `400` is, which differs by family — a rule's scope is
    /// built by the server, while a scorecard or campaign is refused for its own state.
    pub async fn evaluate(
        &self,
        family: EvaluationFamily,
        name: &str,
        body: Option<&Value>,
        origin: BadRequestOrigin,
    ) -> Result<EngineResponse<EvaluationStarted>, ToolError> {
        let url = self.url([
            COMPLIANCE_GROUP,
            COMPLIANCE_VERSION,
            "items",
            family.as_str(),
            name,
            EVALUATE_SEGMENT,
        ])?;

        self.post_json(&EVALUATE_COMPLIANCE, url, body, origin)
            .await
    }
}
