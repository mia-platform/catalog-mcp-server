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
    address::ItemTypeAddress,
    client::{EngineClient, EngineResponse},
    error::{BadRequestOrigin, ToolError},
    models::ListEnvelope,
    ops::{
        DELETE_ITEM_TYPE_DEFINITION, GET_ITEM_TYPE_DEFINITION, LIST_ITEM_TYPE_DEFINITIONS,
        ListQuery, PUT_ITEM_TYPE_DEFINITION,
    },
    pagination::{ListPage, MAX_LIMIT, paginate_all},
    projection::Projection,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Path segments of the Item Type Definition collection.
const ITEM_TYPE_DEFINITION_SEGMENTS: [&str; 3] = ["mia-platform.eu", "v1", "item-type-definitions"];

impl EngineClient {
    /// `GET /mia-platform.eu/v1/item-type-definitions` — the type listing.
    ///
    /// **Generic over the model each definition is read into**, so one operation serves both
    /// of its callers. The coordinate resolver reads the full
    /// [`ItemTypeDefinition`](crate::models::ItemTypeDefinition); `list_catalog_types` reads the
    /// lean [`ItdListEntry`](crate::models::ItdListEntry), which never materialises the
    /// per-version schemas that are ~90 % of the payload. Either way it is one request, one entry
    /// in [`OPERATIONS`](crate::ops::OPERATIONS) and one identity test.
    ///
    /// Always the full projection: the partial one carries no `spec`, so no `kind` and no
    /// versions.
    pub async fn list_item_type_definitions<T: DeserializeOwned>(
        &self,
        query: &ListQuery,
    ) -> Result<EngineResponse<ListPage<T>>, ToolError> {
        let mut url = self.url(ITEM_TYPE_DEFINITION_SEGMENTS)?;
        query.apply(&mut url, LIST_ITEM_TYPE_DEFINITIONS.query);

        let response: EngineResponse<ListEnvelope<T>> = self
            .get_json(
                &LIST_ITEM_TYPE_DEFINITIONS,
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

    /// `GET /mia-platform.eu/v1/item-type-definitions/{name}` — one definition, **as the engine sent
    /// it**.
    ///
    /// Raw by design: a write built from a re-serialised typed copy would erase every field the
    /// model does not declare — a version's `deprecationWarning`, say — because `spec.versions` is
    /// replaced whole.
    pub async fn get_item_type_definition(
        &self,
        address: &ItemTypeAddress,
    ) -> Result<EngineResponse<Value>, ToolError> {
        let url = self.url(address.segments())?;

        // The name is derived from the caller's `group` and `plural`.
        self.get_json(
            &GET_ITEM_TYPE_DEFINITION,
            url,
            Projection::Full.accept(),
            BadRequestOrigin::CallerInput,
        )
        .await
    }

    /// `PUT /mia-platform.eu/v1/item-type-definitions/{name}` — write one definition whole, and read
    /// back what the engine stored, raw.
    ///
    /// `retryable` comes from the write cycle's conflict policy, never from the call site.
    pub async fn put_item_type_definition(
        &self,
        address: &ItemTypeAddress,
        manifest: &Value,
        retryable: bool,
    ) -> Result<EngineResponse<Value>, ToolError> {
        let url = self.url(address.segments())?;

        // The manifest is the caller's: a `400` is a definition it can correct.
        self.put_json(
            &PUT_ITEM_TYPE_DEFINITION,
            url,
            manifest,
            retryable,
            BadRequestOrigin::CallerInput,
        )
        .await
    }

    /// `DELETE /mia-platform.eu/v1/item-type-definitions/{name}?resourceVersion=…` — delete one
    /// definition, and with it every item of the type, their relationships and the constraints
    /// naming it.
    ///
    /// `resource_version` is always sent by `delete_item_type`: without it the engine deletes
    /// whatever is there now. A `204` has no body; when the cascade failed, the engine's warning
    /// rides on the response's warnings. Never retried: a failure after dispatch may have landed.
    pub async fn delete_item_type_definition(
        &self,
        address: &ItemTypeAddress,
        resource_version: Option<&str>,
    ) -> Result<EngineResponse<()>, ToolError> {
        let mut url = self.url(address.segments())?;
        if let Some(version) = resource_version {
            url.query_pairs_mut()
                .append_pair(DELETE_ITEM_TYPE_DEFINITION.query[0], version);
        }

        self.delete_empty(
            &DELETE_ITEM_TYPE_DEFINITION,
            url,
            BadRequestOrigin::CallerInput,
        )
        .await
    }

    /// **Every** type the caller can see, walked to exhaustion under
    /// [`MAX_INTERNAL_PAGES`](crate::pagination::MAX_INTERNAL_PAGES).
    ///
    /// All or nothing: a page that fails fails the walk, because a silently short list makes
    /// real types look nonexistent. Only `limit` — the engine's maximum — and the cursor are
    /// sent, nothing of the caller's. Shared by `list_catalog_types`, which lists the catalogue,
    /// and `search_catalog`, which suggests near matches for a `kind` that does not exist.
    pub async fn list_all_item_type_definitions<T: DeserializeOwned>(
        &self,
    ) -> Result<Vec<T>, ToolError> {
        paginate_all(|cursor| async move {
            self.list_item_type_definitions::<T>(&ListQuery {
                limit: Some(MAX_LIMIT),
                cursor,
                ..ListQuery::default()
            })
            .await
            .map(|response| response.value)
        })
        .await
    }
}
