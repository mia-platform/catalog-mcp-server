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
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "hello";

/// What the tool does. Kept to one line: it is paid for on every conversation.
const TOOL_DESCRIPTION: &str = "Check that the Catalog MCP server is reachable and reporting its version. Takes no \
     arguments and reads nothing from the catalog.";

/// `hello` takes no arguments: the probe must not depend on anything a caller sends.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HelloInput {}

/// The Step 1 probe: proves the handler, the transport and the identity hook with no catalog
/// logic behind them (§13.2).
///
/// It is also the smallest possible worked example of the §5.5 contract — no engine call, no
/// arguments, no warnings — which is why it is worth keeping beside `list_tenants`, the example
/// that does exercise all three.
pub struct Hello;

impl Tool for Hello {
    type Input = HelloInput;

    /// `readOnlyHint: true` is the one hint that differs from the specification's defaults
    /// (D16): omitting annotations entirely would declare this probe destructive and open-world.
    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<HelloInput>(
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
        // The tenant is reported because it is the one thing a reachability probe can usefully
        // confirm beyond "the process answered": that identity reached the tool at all. It comes
        // from the `CallContext`, never from a header — rule 1.
        Ok(ToolOutput::new(json!({
            "server": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "tenant": context.tenant().to_string(),
        })))
    }
}

#[cfg(test)]
mod tests;
