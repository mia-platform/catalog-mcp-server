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
    models::Tenant,
    ops::LIST_TENANTS,
    projection::Projection,
};

/// Path segments of the tenant listing.
const TENANTS_SEGMENTS: [&str; 2] = ["bff", "tenants"];

impl EngineClient {
    /// `GET /bff/tenants` — the tenants the caller can see.
    ///
    /// **Not a catalog read.** The engine proxies it to the Mia-Platform authz service,
    /// optionally exchanging the caller's JWT first, so its failure mode is *authentication*
    /// rather than *data* — which is what makes it the cheapest end-to-end probe of the
    /// identity path in the whole tool set.
    ///
    /// The response is a **bare JSON array**, not a `List` envelope: there is no
    /// `metadata.continue`, no `items` wrapper and nothing to paginate. The endpoint declares no
    /// parameters at all.
    pub async fn list_tenants(&self) -> Result<EngineResponse<Vec<Tenant>>, ToolError> {
        let url = self.url(TENANTS_SEGMENTS)?;

        // The endpoint declares no parameters, so nothing in the request is the caller's.
        self.get_json(
            LIST_TENANTS.id,
            url,
            Projection::Full.accept(),
            BadRequestOrigin::ServerBuilt,
        )
        .await
    }
}
