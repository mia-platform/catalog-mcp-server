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
    models::{ListEnvelope, MeContext, Principal, PrincipalType},
    ops::{GET_ME, LIST_PRINCIPALS},
    pagination::{EngineCursor, ListPage},
    projection::Projection,
};

/// Path segments of the caller's own identity.
const ME_SEGMENTS: [&str; 2] = ["bff", "me"];

/// Path segments of the tenant's principal directory.
const PRINCIPALS_SEGMENTS: [&str; 2] = ["bff", "principals"];

/// The separator of the `id` parameter's list.
const ID_SEPARATOR: &str = ",";

/// How one page of principals is asked for.
///
/// `ids`, when given, must not be empty: the engine reads an empty `id` as *the empty set* and
/// answers nothing, which a caller would read as *nobody*.
#[derive(Clone, Debug, Default)]
pub struct PrincipalQuery {
    /// How many principals to ask for. The engine's own default is 50 and its maximum 200.
    pub limit: Option<u32>,

    /// Where to continue from.
    pub cursor: Option<EngineCursor>,

    /// Free text, matched by authz over name, e-mail and username.
    pub search: Option<String>,

    /// Only users, or only service accounts.
    pub principal_type: Option<PrincipalType>,

    /// Only these principal ids.
    pub ids: Option<Vec<String>>,
}

impl EngineClient {
    /// `GET /bff/me` — the caller, as the authz service identifies the forwarded bearer.
    ///
    /// **Not a catalog read**: the engine proxies it to authz, so its failures are about identity
    /// and authz, as for `/bff/tenants`. It takes no parameters.
    pub async fn get_me(&self) -> Result<EngineResponse<MeContext>, ToolError> {
        let url = self.url(ME_SEGMENTS)?;

        self.get_json(
            &GET_ME,
            url,
            Projection::Full.accept(),
            BadRequestOrigin::ServerBuilt,
        )
        .await
    }

    /// `GET /bff/principals` — one page of the tenant's users and service accounts.
    ///
    /// A pure proxy to authz, which matches `search`, applies `type` and `id`, and refuses a
    /// caller without permission to read principals. The response is a `List` envelope paged by
    /// authz's own cursor. A `400` is ours: every parameter is built and validated by the caller
    /// of this method.
    pub async fn list_principals(
        &self,
        query: &PrincipalQuery,
    ) -> Result<EngineResponse<ListPage<Principal>>, ToolError> {
        let mut url = self.url(PRINCIPALS_SEGMENTS)?;

        {
            let mut pairs = url.query_pairs_mut();
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(cursor) = &query.cursor {
                pairs.append_pair("continue", cursor.as_str());
            }
            if let Some(search) = &query.search {
                pairs.append_pair("search", search);
            }
            if let Some(principal_type) = query.principal_type {
                pairs.append_pair("type", principal_type.as_str());
            }
            if let Some(ids) = &query.ids {
                pairs.append_pair("id", &ids.join(ID_SEPARATOR));
            }
        }

        let response: EngineResponse<ListEnvelope<Principal>> = self
            .get_json(
                &LIST_PRINCIPALS,
                url,
                Projection::Full.accept(),
                BadRequestOrigin::ServerBuilt,
            )
            .await?;

        Ok(EngineResponse {
            value: ListPage::from_envelope(response.value),
            warnings: response.warnings,
        })
    }
}
