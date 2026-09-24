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
use crate::registry::{
    ToolDescriptor,
    contract::{CallContext, Tool, ToolOutput},
};
use catalog_client::ToolError;
use rmcp::model::ToolAnnotations;
use serde::Deserialize;
use serde_json::{Value, json};

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "list_tenants";

/// What the tool does.
///
/// It says both halves on purpose: the list is the informative part, and **which one you are in**
/// is the useful part (T11-D2). A model that knows it is in the production catalog can say so
/// before doing something regrettable.
const TOOL_DESCRIPTION: &str =
    "List the tenants you can access, and which one you are currently working in.";

/// `list_tenants` takes no arguments (T11-D1).
///
/// **Deliberately.** The result is scoped by the caller's identity and nothing else. There is no
/// filter the model could usefully apply, and offering one would invite it to believe it can
/// widen its own scope — the opposite of what NFR-01 wants a tool surface to suggest.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListTenantsInput {}

/// The worked example of the §5.5 contract, and the cheapest end-to-end probe of the identity
/// path in the whole tool set (§13.5, T11).
///
/// **Its natural failure is `401`, not `404`.** `GET /bff/tenants` is not a catalog read: the
/// engine proxies it to the authz service. So if this tool works, identity is flowing through
/// the gateway, the exchange and the engine; if it `401`s while other tools work, the auth
/// wiring is broken and the catalog is not.
///
/// And one verified fact makes it sharper: `/api/catalog/bff/tenants` is one of three routes
/// carrying `ExtAuthzPerRoute` with the filter **disabled**, so no policy runs on that hop —
/// the headers *we* forward are exactly what the engine sees. On every other route the policy
/// regenerates them (D48), so this is the only tool whose success proves **D26's forwarding**
/// rather than proving the policy works.
pub struct ListTenants;

impl Tool for ListTenants {
    type Input = ListTenantsInput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<ListTenantsInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().read_only(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        _input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        let response = context.engine().list_tenants().await?;

        let tenants: Vec<Value> = response
            .value
            .iter()
            .map(|tenant| {
                json!({
                    // The engine's `name` is the slug — the value an ACL context carries — and
                    // its `title` is the display name. Reporting them under these names is what
                    // makes `current` comparable to an entry in the list.
                    "id": tenant.name,
                    "name": tenant.title,
                    "organization": tenant.organization,
                })
            })
            .collect();

        let mut payload = json!({ "tenants": tenants });

        // T11-D2 — `current` comes from the forwarded ACL context, never from a second call, and
        // is **omitted rather than null** when the context did not decode. Under D47 the
        // identity layer extracts rather than rejects, so that is a reachable state and the tool
        // must not invent a value for it.
        if let Some(current) = current_tenant(context) {
            payload["current"] = json!(current);
        }

        // T11-D5 — an empty list is a real, successful answer, and materially different from a
        // `401`. Saying so plainly is what stops a model proceeding as though it had a scope.
        Ok(ToolOutput::new(payload))
    }
}

/// The tenant the caller is acting in, from the forwarded ACL context.
///
/// `None` when no usable context arrived — which is not an error here, and not this server's to
/// adjudicate (D47).
fn current_tenant(context: &CallContext) -> Option<&str> {
    let tenant = context.tenant();

    (tenant.tenant != catalog_client::identity::UNKNOWN_TENANT).then_some(tenant.tenant.as_str())
}

#[cfg(test)]
mod tests;
