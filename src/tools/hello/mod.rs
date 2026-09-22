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
    handler::{CatalogHandler, inbound_parts},
    registry::ToolDescriptor,
};
use rmcp::{
    handler::server::router::tool::ToolRoute,
    model::{CallToolResponse, CallToolResult, ContentBlock, Tool, ToolAnnotations},
};
use serde::Deserialize;
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "hello";

/// What the tool does. Kept to one line: it is paid for on every conversation.
const TOOL_DESCRIPTION: &str = "Check that the Catalog MCP server is reachable and reporting its version. Takes no \
     arguments and reads nothing from the catalog.";

/// Inbound header carrying the request correlation id, minted by the gateway or by this
/// server's own request-id layer. Tier 2 of the D26 allowlist, and not sensitive.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// `hello` takes no arguments (§5.5): the probe must not depend on anything a caller sends.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HelloInput {}

/// The static description of `hello`, including its annotations.
///
/// `readOnlyHint: true` is the one hint that differs from the specification's defaults (D16):
/// omitting annotations entirely would declare this probe destructive and open-world.
pub fn descriptor() -> ToolDescriptor {
    ToolDescriptor::new::<HelloInput>(
        TOOL_NAME,
        TOOL_DESCRIPTION,
        ToolAnnotations::new().read_only(true),
    )
}

/// The registry route for `hello`.
///
/// Registered through [`ToolRoute::new_dyn`] rather than the `#[tool]` macro, which is what
/// keeps the descriptor — and therefore the byte budget — ours (§5.4).
pub fn route() -> ToolRoute<CatalogHandler> {
    let tool: Tool = (&descriptor()).into();

    ToolRoute::new_dyn(tool, |context| {
        Box::pin(async move {
            // §6.2, and the whole reason this tool exists in Step 1: the transport injects the
            // inbound `http::request::Parts` into the request context's extensions, so a
            // forwarded header is reachable from inside a tool call. The contract has no
            // upstream integration test, so ours proves it on both protocol eras.
            let request_id = inbound_parts(context.request_context())
                .and_then(|parts| parts.headers.get(REQUEST_ID_HEADER))
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);

            let payload = json!({
                "server": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
                "requestId": request_id,
            });

            let text =
                serde_json::to_string(&payload).expect("the hello payload is always serialisable");

            Ok(CallToolResponse::from(CallToolResult::success(vec![
                ContentBlock::text(text),
            ])))
        })
    })
}

#[cfg(test)]
mod tests;
