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
    address::ItemAddress,
    client::{EngineClient, EngineResponse},
    error::{BadRequestOrigin, ToolError},
    models::{ItemRelationshipEntry, ListEnvelope, RelationshipDirection},
    ops::GET_RELATIONSHIPS,
    pagination::{EngineCursor, ListPage},
    projection::Projection,
};

/// The path segments around an item's address that make its relationships' URL.
const BFF_SEGMENT: &str = "bff";
const RELATIONSHIPS_SEGMENT: &str = "relationships";

/// How one page of relationships is asked for. Deliberately without a `groupBy` or a `rawq`
/// knob: this client cannot send either (T3-D1, T3-D7).
#[derive(Clone, Debug, Default)]
pub struct RelationshipQuery {
    /// How many entries to ask for. The engine's own default is 50 and its maximum 200.
    pub limit: Option<u32>,

    /// Where to continue from.
    pub cursor: Option<EngineCursor>,

    /// Only the entries in which the item is at this end.
    pub direction: Option<RelationshipDirection>,
}

impl EngineClient {
    /// `GET /bff/{group}/{version}/items/{family}/{name}/relationships` — one page of an item's
    /// relationships, flat, with the other ends in the metadata-only projection.
    ///
    /// A `400` is reported as **ours**: the caller cannot compose this query — `direction` is a
    /// typed choice and everything else is paging.
    pub async fn get_relationships(
        &self,
        item: &ItemAddress,
        query: &RelationshipQuery,
    ) -> Result<EngineResponse<ListPage<ItemRelationshipEntry>>, ToolError> {
        let [group, version, items, family, name] = item.segments();
        let mut url = self.url([
            BFF_SEGMENT,
            group,
            version,
            items,
            family,
            name,
            RELATIONSHIPS_SEGMENT,
        ])?;

        {
            let mut pairs = url.query_pairs_mut();
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(cursor) = &query.cursor {
                pairs.append_pair("continue", cursor.as_str());
            }
            if let Some(direction) = query.direction {
                pairs.append_pair("direction", direction.as_str());
            }
        }

        let response: EngineResponse<ListEnvelope<ItemRelationshipEntry>> = self
            .get_json(
                GET_RELATIONSHIPS.id,
                url,
                Projection::PartialObjectMetadata.accept(),
                BadRequestOrigin::ServerBuilt,
            )
            .await?;

        Ok(EngineResponse {
            value: ListPage::from_envelope(response.value),
            warnings: response.warnings,
        })
    }
}
