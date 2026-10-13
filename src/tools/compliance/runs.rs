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
// Reading compliance items and finding runs. Every query here is built by this server — the
// filters and the newest-first `sort` alike — so a `400` on one is ours, never the caller's.

use super::{Target, compliance_address};
use crate::tools::lookup::no_such_item;
use catalog_client::{
    EngineClient, FamilyAddress, FieldPath, Predicate, QueryValue, Remedy, ToolError,
    error::codes,
    models::{Item, ObjectMetadata, PartialObjectMetadata},
    ops::{
        ListQuery,
        compliance::{COMPLIANCE_GROUP, COMPLIANCE_VERSION},
    },
};
use serde::de::DeserializeOwned;

/// Newest first: what "the latest run" means.
const NEWEST_FIRST: &str = "-metadata.creationTimestamp";

/// A ScorecardEvaluation's state, as filtered on.
const EVALUATION_STATUS_FIELD: &str = "spec.evaluation.status";

/// A RuleRun's end time, set exactly when it is finished.
const RUN_END_TIME_FIELD: &str = "spec.runEndTime";

/// One compliance item, read whole, with its `spec` as `T`.
pub struct Read<T> {
    /// The `spec`, typed.
    pub spec: T,

    /// The item's metadata.
    pub metadata: ObjectMetadata,

    /// The item's `resourceVersion`, which a poll compares — for equality only.
    pub resource_version: Option<String>,
}

/// Reads `family/name` in the compliance group and types its `spec`.
///
/// A `spec` that does not read as `T` means the engine's shape and ours have diverged: that is ours
/// to fix, so it is a `server_defect`, never an error the model could correct.
pub async fn read<T: DeserializeOwned>(
    engine: &EngineClient,
    family: &str,
    name: &str,
) -> Result<Read<T>, ToolError> {
    let address = compliance_address(family, name)?;
    let item = engine.get_item(&address).await?.value;

    typed(item)
}

/// Types an item already read.
pub fn typed<T: DeserializeOwned>(item: Item) -> Result<Read<T>, ToolError> {
    let spec = serde_json::from_value(item.spec).map_err(|err| {
        tracing::error!(?err, name = %item.metadata.name, "a compliance item this server cannot read");
        ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            format!(
                "The catalog returned a `{}` this server cannot read.",
                item.kind
            ),
        )
    })?;

    Ok(Read {
        spec,
        metadata: item.metadata,
        resource_version: item.resource_version,
    })
}

/// Reads the target itself, answering an unknown name with near matches, as every tool does.
pub async fn read_target<T: DeserializeOwned>(
    engine: &EngineClient,
    target: Target,
    name: &str,
) -> Result<Read<T>, ToolError> {
    match read(engine, target.family(), name).await {
        Err(error) if error.code == codes::NOT_FOUND => Err(no_such_item(engine, name).await),
        other => other,
    }
}

/// The `resourceVersion` of `family/name`, read from its metadata alone.
pub async fn read_version(
    engine: &EngineClient,
    family: &str,
    name: &str,
) -> Result<Option<String>, ToolError> {
    let address = compliance_address(family, name)?;

    Ok(engine
        .get_item_metadata(&address)
        .await?
        .value
        .resource_version)
}

/// The runs of `target` named `name` matching `condition`, newest first, at most `limit`.
///
/// `full` reads each run whole — what counts need — rather than its metadata alone.
pub async fn list_runs(
    engine: &EngineClient,
    target: Target,
    name: &str,
    condition: RunCondition,
    limit: u32,
) -> Result<Vec<Item>, ToolError> {
    let (family, query) = run_query(target, name, condition, limit)?;

    Ok(engine.list_family_items(&family, &query).await?.value.items)
}

/// The newest run of `target` named `name` matching `condition`, metadata only.
pub async fn newest_run(
    engine: &EngineClient,
    target: Target,
    name: &str,
    condition: RunCondition,
) -> Result<Option<PartialObjectMetadata>, ToolError> {
    let (family, query) = run_query(target, name, condition, 1)?;

    Ok(engine
        .list_family_items_partial(&family, &query)
        .await?
        .value
        .items
        .into_iter()
        .next())
}

/// Which runs of a target to list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunCondition {
    /// Every run.
    Any,

    /// Finished runs: `complete` or `error`.
    Finished,

    /// Runs still pending.
    Pending,
}

/// The family and the query listing `target`'s runs under `condition`.
fn run_query(
    target: Target,
    name: &str,
    condition: RunCondition,
    limit: u32,
) -> Result<(FamilyAddress, ListQuery), ToolError> {
    let (Some(run_family), Some(reference_field)) =
        (target.run_family(), target.run_reference_field())
    else {
        return Err(no_runs(target));
    };

    let mut clauses = vec![Predicate::Eq {
        field: FieldPath::new(reference_field)?,
        value: QueryValue::string(&target.urn(name))?,
    }];

    match (target, condition) {
        (_, RunCondition::Any) => {}
        (Target::Rule, RunCondition::Finished) => clauses.push(Predicate::Present {
            field: FieldPath::new(RUN_END_TIME_FIELD)?,
        }),
        (Target::Rule, RunCondition::Pending) => clauses.push(Predicate::Missing {
            field: FieldPath::new(RUN_END_TIME_FIELD)?,
        }),
        // `or` rather than `not_eq`: every operator here is one already proven against the engine.
        (_, RunCondition::Finished) => clauses.push(Predicate::Or(vec![
            status_is("complete")?,
            status_is("error")?,
        ])),
        (_, RunCondition::Pending) => clauses.push(status_is("pending")?),
    }

    let family = FamilyAddress::new(COMPLIANCE_GROUP, COMPLIANCE_VERSION, run_family)?;
    let query = ListQuery {
        limit: Some(limit),
        raw_query: Predicate::And(clauses).encode_rawq()?,
        sort: vec![NEWEST_FIRST.to_string()],
        sort_is_ours: true,
        ..ListQuery::default()
    };

    Ok((family, query))
}

/// `spec.evaluation.status eq <status>`.
fn status_is(status: &str) -> Result<Predicate, ToolError> {
    Ok(Predicate::Eq {
        field: FieldPath::new(EVALUATION_STATUS_FIELD)?,
        value: QueryValue::string(status)?,
    })
}

/// A campaign keeps no runs: asking for them is a defect of the caller's code, not the model's.
fn no_runs(target: Target) -> ToolError {
    ToolError::new(
        codes::SERVER_DEFECT,
        Remedy::Escalate,
        format!("A `{}` keeps no runs to list.", target.kind()),
    )
}
