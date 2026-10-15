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
// The audit log: one offset-addressed listing, and the vocabulary of the actions it records.

use crate::{
    client::{EngineClient, EngineResponse},
    error::{BadRequestOrigin, ToolError},
    models::{AuditAction, AuditLogPage},
    ops::{LIST_AUDIT_LOG, LIST_AUDIT_LOG_ACTIONS},
    projection::Projection,
};

/// The path segment of the audit log.
const AUDIT_LOG_SEGMENT: &str = "audit-log";

/// The path segment of its vocabulary.
const ACTIONS_SEGMENT: &str = "actions";

/// How one page of the audit log is asked for. Every field is sent as the engine names it;
/// `includeAuditLogReads` has no field, so it can never be sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuditLogQuery {
    /// `occurredSince`: entries at or after this instant.
    pub since: Option<String>,

    /// `occurredUntil`: entries strictly before this instant.
    pub until: Option<String>,

    /// `principal`, repeated: entries attributed to one of these principal ids.
    pub principals: Vec<String>,

    /// `action`, repeated: entries recording one of these action codes.
    pub actions: Vec<String>,

    /// `outcome`: `success` or `error`.
    pub outcome: Option<String>,

    /// `scope`: `instance`, `collection` or `none`.
    pub scope: Option<String>,

    /// `resourceUrn`: entries naming this resource.
    pub resource_urn: Option<String>,

    /// `resourceTypeUrn`: entries naming a resource of this type, by the type definition's URN.
    pub resource_type_urn: Option<String>,

    /// `skip`: how many matching entries come before the page.
    pub skip: u64,

    /// `limit`: the page size.
    pub limit: u32,
}

impl EngineClient {
    /// `GET /audit-log` — one page of the tenant's audit log, newest first.
    ///
    /// `origin` says whose fault a `400` is: the caller's filters on a first page, this server's
    /// pinned window on a later one.
    pub async fn list_audit_log(
        &self,
        query: &AuditLogQuery,
        origin: BadRequestOrigin,
    ) -> Result<EngineResponse<AuditLogPage>, ToolError> {
        let mut url = self.url([AUDIT_LOG_SEGMENT])?;

        {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in [
                ("occurredSince", &query.since),
                ("occurredUntil", &query.until),
                ("outcome", &query.outcome),
                ("scope", &query.scope),
                ("resourceUrn", &query.resource_urn),
                ("resourceTypeUrn", &query.resource_type_urn),
            ] {
                if let Some(value) = value {
                    pairs.append_pair(name, value);
                }
            }
            for principal in &query.principals {
                pairs.append_pair("principal", principal);
            }
            for action in &query.actions {
                pairs.append_pair("action", action);
            }
            pairs.append_pair("skip", &query.skip.to_string());
            pairs.append_pair("limit", &query.limit.to_string());
        }

        self.get_json(&LIST_AUDIT_LOG, url, Projection::Full.accept(), origin)
            .await
    }

    /// `GET /audit-log/actions` — every action code the engine can record, with its labels.
    pub async fn list_audit_log_actions(
        &self,
    ) -> Result<EngineResponse<Vec<AuditAction>>, ToolError> {
        let url = self.url([AUDIT_LOG_SEGMENT, ACTIONS_SEGMENT])?;

        self.get_json(
            &LIST_AUDIT_LOG_ACTIONS,
            url,
            Projection::Full.accept(),
            BadRequestOrigin::ServerBuilt,
        )
        .await
    }
}
