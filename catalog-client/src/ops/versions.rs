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
// An item's history: its newest revisions, one revision's manifest, its versions, tagging one
// revision as a version, and restoring one. Written once here for every tool that reads or writes
// history.

use crate::{
    address::ItemAddress,
    client::{EngineClient, EngineResponse},
    error::{BadRequestOrigin, ToolError},
    models::{ItemRevision, ItemVersion, ListEnvelope},
    ops::{CREATE_VERSION, GET_REVISION, LIST_REVISIONS, LIST_VERSIONS, RESTORE_VERSION},
    pagination::{EngineCursor, ListPage},
    projection::Projection,
};
use serde_json::{Value, json};

/// The path segment of an item's revisions.
const REVISIONS_SEGMENT: &str = "revisions";

/// The path segment of an item's versions.
const VERSIONS_SEGMENT: &str = "versions";

/// The path segment that restores a version.
const RESTORE_SEGMENT: &str = "restore";

impl EngineClient {
    /// `GET …/{name}/revisions` — the newest `limit` revisions of an item, newest first: the order
    /// is fixed by the engine.
    ///
    /// With the type's recording off the engine still answers, with a `Warning` that the history is
    /// frozen; the warning rides on the response.
    pub async fn list_revisions(
        &self,
        address: &ItemAddress,
        limit: u32,
    ) -> Result<EngineResponse<ListPage<ItemRevision>>, ToolError> {
        let [group, version, items, family, name] = address.segments();
        let mut url = self.url([group, version, items, family, name, REVISIONS_SEGMENT])?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());

        let response: EngineResponse<ListEnvelope<ItemRevision>> = self
            .get_json(
                &LIST_REVISIONS,
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

    /// `GET …/{name}/revisions/{uid}` — the item's manifest as one revision recorded it, raw: the
    /// same shape as the item's own read, without `resourceVersion`.
    pub async fn get_revision(
        &self,
        address: &ItemAddress,
        uid: &str,
    ) -> Result<EngineResponse<Value>, ToolError> {
        let [group, version, items, family, name] = address.segments();
        let url = self.url([group, version, items, family, name, REVISIONS_SEGMENT, uid])?;

        // Only ever a uid the engine itself listed.
        self.get_json(
            &GET_REVISION,
            url,
            Projection::Full.accept(),
            BadRequestOrigin::ServerBuilt,
        )
        .await
    }

    /// `GET …/{name}/versions` — one page of an item's versions, highest SemVer first.
    pub async fn list_versions(
        &self,
        address: &ItemAddress,
        limit: u32,
        cursor: Option<&EngineCursor>,
    ) -> Result<EngineResponse<ListPage<ItemVersion>>, ToolError> {
        let [group, version, items, family, name] = address.segments();
        let mut url = self.url([group, version, items, family, name, VERSIONS_SEGMENT])?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("limit", &limit.to_string());
            if let Some(cursor) = cursor {
                pairs.append_pair("continue", cursor.as_str());
            }
        }

        let response: EngineResponse<ListEnvelope<ItemVersion>> = self
            .get_json(
                &LIST_VERSIONS,
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

    /// `POST …/{name}/versions` — tag `revision_id` of the item as `name`.
    ///
    /// **Never retried**, as every `POST`: tagging is create-exactly-once, so a repeat after a lost
    /// answer would meet the tag the first attempt made.
    pub async fn create_version(
        &self,
        address: &ItemAddress,
        name: &str,
        revision_id: &str,
        description: Option<&str>,
    ) -> Result<EngineResponse<ItemVersion>, ToolError> {
        let [group, version, items, family, item] = address.segments();
        let url = self.url([group, version, items, family, item, VERSIONS_SEGMENT])?;

        let mut body = json!({ "name": name, "revisionId": revision_id });
        if let Some(description) = description {
            body["description"] = json!(description);
        }

        // The name, the revision and the description are the caller's.
        self.post_json(
            &CREATE_VERSION,
            url,
            Some(&body),
            BadRequestOrigin::CallerInput,
        )
        .await
    }

    /// `POST …/{name}/versions/{versionName}/restore?resourceVersion=…` — rewrite the item with
    /// the version's manifest, **with no body**; the answer is the item as it now stands, raw.
    ///
    /// **Never retried**, as every `POST`. `resource_version` guards the write on the state the
    /// caller read; the engine checks it only at the final write.
    pub async fn restore_version(
        &self,
        address: &ItemAddress,
        version_name: &str,
        resource_version: Option<&str>,
    ) -> Result<EngineResponse<Value>, ToolError> {
        let [group, version, items, family, name] = address.segments();
        let mut url = self.url([
            group,
            version,
            items,
            family,
            name,
            VERSIONS_SEGMENT,
            version_name,
            RESTORE_SEGMENT,
        ])?;
        if let Some(resource_version) = resource_version {
            url.query_pairs_mut()
                .append_pair("resourceVersion", resource_version);
        }

        // The version name is the caller's.
        self.post_json(&RESTORE_VERSION, url, None, BadRequestOrigin::CallerInput)
            .await
    }
}
