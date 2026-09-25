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
    error::ToolError,
    models::ListEnvelope,
    ops::{LIST_ITEM_TYPE_DEFINITIONS, ListQuery},
    pagination::ListPage,
    projection::Projection,
};
use serde::de::DeserializeOwned;

/// Path segments of the Item Type Definition collection.
const ITEM_TYPE_DEFINITION_SEGMENTS: [&str; 3] = ["mia-platform.eu", "v1", "item-type-definitions"];

impl EngineClient {
    /// `GET /mia-platform.eu/v1/item-type-definitions` — the type listing.
    ///
    /// **Generic over the model each definition is read into**, so one operation serves both
    /// of its callers. The coordinate resolver (§8.6) reads the full
    /// [`ItemTypeDefinition`](crate::models::ItemTypeDefinition); T1 reads the lean
    /// [`ItdListEntry`](crate::models::ItdListEntry), which never materialises the per-version
    /// schemas that are ~90 % of the payload (T1-D3). Either way it is one request, one entry in
    /// [`OPERATIONS`](crate::ops::OPERATIONS) and one identity test.
    ///
    /// Always the full projection: the partial one carries no `spec`, so no `kind` and no
    /// versions (T1 §5).
    pub async fn list_item_type_definitions<T: DeserializeOwned>(
        &self,
        query: &ListQuery,
    ) -> Result<EngineResponse<ListPage<T>>, ToolError> {
        let mut url = self.url(ITEM_TYPE_DEFINITION_SEGMENTS)?;
        query.apply(&mut url, LIST_ITEM_TYPE_DEFINITIONS.query);

        let response: EngineResponse<ListEnvelope<T>> = self
            .get_json(
                LIST_ITEM_TYPE_DEFINITIONS.id,
                url,
                Projection::Full.accept(),
                query.bad_request_origin(),
            )
            .await?;

        Ok(EngineResponse {
            value: ListPage::from_envelope(response.value),
            warnings: response.warnings,
        })
    }
}
