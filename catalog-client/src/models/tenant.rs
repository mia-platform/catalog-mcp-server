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
use serde::Deserialize;

/// A tenant as the authz service describes it, proxied by the engine's `/bff/tenants`.
///
/// **Note the naming, because it is a genuine trap.** The engine's `name` is the *slug* — the
/// value that appears in an ACL context's `tenant` — and its `title` is the display name. T11
/// therefore reports the slug as `id` and the title as `name`, which is what makes its `current`
/// field comparable to the ACL context.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct Tenant {
    /// The tenant slug. Matches an ACL context's `tenant`.
    #[serde(rename = "name")]
    pub name: String,

    /// The organization the tenant belongs to.
    #[serde(rename = "organization")]
    pub organization: String,

    /// The display name.
    #[serde(rename = "title")]
    pub title: String,

    /// A description, when the authz service supplies one.
    #[serde(
        rename = "description",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<String>,
}
