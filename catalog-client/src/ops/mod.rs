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
    models::{Item, ListEnvelope, PartialObjectMetadata},
    pagination::{EngineCursor, ListPage},
    projection::Projection,
};
use url::Url;

/// One engine operation this client wraps.
///
/// The list exists so that **one** enumeration drives the NFR-11 propagation test: an operation
/// added without forwarding the identity pair fails CI rather than being noticed later. It is
/// also the inventory of every engine endpoint this client depends on — what to read first when
/// the engine version the chart deploys changes, since there is no vendored OAS to diff against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationSpec {
    /// How the operation is named in logs, metrics and test failures.
    pub id: &'static str,

    /// The HTTP method.
    pub method: &'static str,

    /// The OAS path template, exactly as the engine declares it.
    pub path: &'static str,

    /// Every query parameter this client may send on the operation.
    pub query: &'static [&'static str],
}

/// The global item listing.
pub const LIST_ITEMS: OperationSpec = OperationSpec {
    id: "list_items",
    method: "get",
    path: "/items",
    query: &["limit", "continue", "rawq", "sort"],
};

/// One item, by address.
pub const GET_ITEM: OperationSpec = OperationSpec {
    id: "get_item",
    method: "get",
    path: "/{group}/{version}/items/{family}/{name}",
    query: &[],
};

/// One item, written whole.
pub const PUT_ITEM: OperationSpec = OperationSpec {
    id: "put_item",
    method: "put",
    path: "/{group}/{version}/items/{family}/{name}",
    query: &[],
};

/// The tenants the caller can see. **Not a catalog read** — the engine proxies it to authz.
pub const LIST_TENANTS: OperationSpec = OperationSpec {
    id: "list_tenants",
    method: "get",
    path: "/bff/tenants",
    query: &[],
};

/// The Item Type Definition listing.
pub const LIST_ITEM_TYPE_DEFINITIONS: OperationSpec = OperationSpec {
    id: "list_item_type_definitions",
    method: "get",
    path: "/mia-platform.eu/v1/item-type-definitions",
    query: &["limit", "continue", "field", "label", "rawq", "sort"],
};

/// Every operation this client wraps today.
///
/// Tool waves add to it; nothing else does.
pub const OPERATIONS: &[OperationSpec] = &[LIST_ITEMS, GET_ITEM, LIST_ITEM_TYPE_DEFINITIONS];

/// How a listing is narrowed and paged.
///
/// Deliberately not a builder: there are four knobs, they are all optional, and a struct with
/// [`Default`] reads better at the call site than four `Option` arguments.
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    /// How many objects to ask for. The engine's own default is 50 and its maximum 200.
    pub limit: Option<u32>,

    /// Where to continue from.
    pub cursor: Option<EngineCursor>,

    /// Pre-encoded `rawq` parameters, AND-ed by the engine. Built by the translator, never here.
    pub raw_query: Vec<String>,

    /// Sort expressions, in the engine's own syntax.
    pub sort: Vec<String>,

    /// `field=<selector>=<value>` filters, for the endpoints that accept them.
    pub field: Vec<String>,

    /// `label=<key>=<value>` filters, for the endpoints that accept them.
    pub label: Vec<String>,
}

impl ListQuery {
    /// Whose fault a `400` on this listing would be (§8.4).
    ///
    /// `field`, `label` and `sort` carry what the caller asked for. `limit`, the cursor and
    /// `rawq` are ours — `rawq` is built by the query translator and never supplied — so a listing
    /// carrying none of the caller's is one the engine can only have rejected because of us, and
    /// telling the model to change its input would send it round a loop it cannot win.
    pub(crate) fn bad_request_origin(&self) -> BadRequestOrigin {
        if self.field.is_empty() && self.label.is_empty() && self.sort.is_empty() {
            BadRequestOrigin::ServerBuilt
        } else {
            BadRequestOrigin::CallerInput
        }
    }

    /// Applies the query to a URL.
    ///
    /// `acl-filter` is **never** added: it is policy-injected, a second occurrence is a `400`,
    /// and a client must never send it.
    pub(crate) fn apply(&self, url: &mut Url, allowed: &[&str]) {
        let mut query = url.query_pairs_mut();

        if let Some(limit) = self.limit
            && allowed.contains(&"limit")
        {
            query.append_pair("limit", &limit.to_string());
        }

        if let Some(cursor) = &self.cursor
            && allowed.contains(&"continue")
        {
            query.append_pair("continue", cursor.as_str());
        }

        if allowed.contains(&"rawq") {
            for raw in &self.raw_query {
                query.append_pair("rawq", raw);
            }
        }

        if allowed.contains(&"sort") {
            for sort in &self.sort {
                query.append_pair("sort", sort);
            }
        }

        if allowed.contains(&"field") {
            for field in &self.field {
                query.append_pair("field", field);
            }
        }

        if allowed.contains(&"label") {
            for label in &self.label {
                query.append_pair("label", label);
            }
        }
    }
}

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

    /// `GET /{group}/{version}/items/{family}/{name}` — one item, by address.
    pub async fn get_item(&self, address: &ItemAddress) -> Result<EngineResponse<Item>, ToolError> {
        let url = self.url(address.segments())?;

        // The address is the caller's, validated but still theirs.
        self.get_json(
            GET_ITEM.id,
            url,
            Projection::Full.accept(),
            BadRequestOrigin::CallerInput,
        )
        .await
    }

    /// `PUT /{group}/{version}/items/{family}/{name}` — write one item whole.
    ///
    /// `retryable` comes from the write cycle's [`ConflictPolicy`](crate::write::ConflictPolicy),
    /// never from the call site: whether a write may be repeated is a property of the intent,
    /// stated once (D23).
    pub async fn put_item(
        &self,
        address: &ItemAddress,
        manifest: &serde_json::Value,
        retryable: bool,
    ) -> Result<EngineResponse<Item>, ToolError> {
        let url = self.url(address.segments())?;

        // The manifest is the caller's: a `400` is a schema failure it can correct (§8.4).
        self.put_json(
            PUT_ITEM.id,
            url,
            manifest,
            retryable,
            BadRequestOrigin::CallerInput,
        )
        .await
    }
}

/// The Item Type Definition listing, generic over the model it is read into.
pub mod item_type_definitions;

/// The tenant listing, which is not a catalog read at all.
pub mod tenants;

#[cfg(test)]
mod tests;
