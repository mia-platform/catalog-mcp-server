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

/// The tool name.
pub const TOOL_NAME: &str = "echo_identity";

/// What the tool does.
const TOOL_DESCRIPTION: &str = "Report the tenant this call reached the tool with. Tests only.";

/// The probe takes no arguments: what it reports must not depend on anything a caller sends.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EchoIdentityInput {}

/// **A test-only probe, never registered in production** (F-10). It is what `hello` was during
/// Step 1 (§13.2): a tool with no catalog logic behind it, reporting the identity that reached
/// it, so the handler, both transport eras and the identity hook can be tested without an
/// engine. The shipped set is the catalog tools alone.
pub struct EchoIdentity;

impl Tool for EchoIdentity {
    type Input = EchoIdentityInput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<EchoIdentityInput>(
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
        // It comes from the `CallContext`, never from a header — rule 1.
        Ok(ToolOutput::new(json!({
            "server": env!("CARGO_PKG_NAME"),
            "version": crate::VERSION,
            "tenant": context.tenant().to_string(),
        })))
    }
}
