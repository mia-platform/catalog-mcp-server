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
    address::FamilyAddress,
    client::{EngineClient, EngineResponse},
    error::ToolError,
    models::{Count, Item, ListEnvelope, PartialObjectMetadata},
    ops::{COUNT_FAMILY_ITEMS, COUNT_ITEMS, LIST_FAMILY_ITEMS, LIST_ITEMS, ListQuery},
    pagination::ListPage,
    projection::Projection,
};

/// The path segment the count operations append to their collection.
const COUNT_SEGMENT: &str = "count";

impl EngineClient {
    /// `GET /items` — the global item listing.
    ///
    /// **The four core families are excluded by the engine** — `Relationship`,
    /// `RelationshipType`, `RelationshipConstraint` and `CustomField` — so a global search never
    /// returns them, and custom-field discovery must use the family-scoped endpoint instead.
    pub async fn list_items(
        &self,
        query: &ListQuery,
    ) -> Result<EngineResponse<ListPage<Item>>, ToolError> {
        let mut url = self.url(["items"])?;
        query.apply(&mut url, LIST_ITEMS.query);

        let response: EngineResponse<ListEnvelope<Item>> = self
            .get_json(
                LIST_ITEMS.id,
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

    /// `GET /items` with the metadata-only projection, which is what a search wants.
    pub async fn list_items_partial(
        &self,
        query: &ListQuery,
    ) -> Result<EngineResponse<ListPage<PartialObjectMetadata>>, ToolError> {
        let mut url = self.url(["items"])?;
        query.apply(&mut url, LIST_ITEMS.query);

        let response: EngineResponse<ListEnvelope<PartialObjectMetadata>> = self
            .get_json(
                LIST_ITEMS.id,
                url,
                Projection::PartialObjectMetadata.accept(),
                query.bad_request_origin(),
            )
            .await?;

        Ok(EngineResponse {
            value: ListPage::from_envelope(response.value),
            warnings: response.warnings,
        })
    }

    /// `GET /{group}/{version}/items/{family}` with the metadata-only projection — one family's
    /// items, which is what a search restricted to a `kind` wants (T2-D2).
    pub async fn list_family_items_partial(
        &self,
        family: &FamilyAddress,
        query: &ListQuery,
    ) -> Result<EngineResponse<ListPage<PartialObjectMetadata>>, ToolError> {
        let mut url = self.url(family.segments())?;
        query.apply(&mut url, LIST_FAMILY_ITEMS.query);

        let response: EngineResponse<ListEnvelope<PartialObjectMetadata>> = self
            .get_json(
                LIST_FAMILY_ITEMS.id,
                url,
                Projection::PartialObjectMetadata.accept(),
                query.bad_request_origin(),
            )
            .await?;

        Ok(EngineResponse {
            value: ListPage::from_envelope(response.value),
            warnings: response.warnings,
        })
    }

    /// `GET /items/count` — how many items match across every type.
    ///
    /// Takes the **same** [`ListQuery`] as the listing it counts, so a count built from one
    /// query cannot disagree with the page it describes (T2 §5). Only `rawq` is sent.
    pub async fn count_items(&self, query: &ListQuery) -> Result<EngineResponse<Count>, ToolError> {
        let mut url = self.url(["items", COUNT_SEGMENT])?;
        query.apply(&mut url, COUNT_ITEMS.query);

        self.get_json(
            COUNT_ITEMS.id,
            url,
            Projection::Full.accept(),
            query.bad_request_origin(),
        )
        .await
    }

    /// `GET /{group}/{version}/items/{family}/count` — how many of one family's items match.
    pub async fn count_family_items(
        &self,
        family: &FamilyAddress,
        query: &ListQuery,
    ) -> Result<EngineResponse<Count>, ToolError> {
        let [group, version, items, family] = family.segments();
        let mut url = self.url([group, version, items, family, COUNT_SEGMENT])?;
        query.apply(&mut url, COUNT_FAMILY_ITEMS.query);

        self.get_json(
            COUNT_FAMILY_ITEMS.id,
            url,
            Projection::Full.accept(),
            query.bad_request_origin(),
        )
        .await
    }
}
