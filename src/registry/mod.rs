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
    handler::{CatalogHandler, caller_identity},
    registry::contract::{
        CallContext, ProgressSink, Tool as CatalogTool, invalid_arguments, success_result,
    },
    schema::minify_input_schema,
    tools,
};
use rmcp::{
    handler::server::{
        common::schema_for_input,
        router::tool::{ToolRoute, ToolRouter},
    },
    model::{CallToolResponse, JsonObject, Tool, ToolAnnotations},
};
use std::sync::Arc;

/// The tool-authoring contract, frozen at the Step 4 gate (§5.5, §13.5).
pub mod contract;

/// Byte allowance per registered tool for the whole `tools/list` payload (D12, §9).
///
/// The ceiling is **`PER_TOOL_ALLOWANCE × tool count`**, not a constant. A constant would
/// eventually fail because the server does *more*, creating pressure to drop a tool to stay
/// under a number — the exact trade §9 rejects. The allowance is deliberately loose, roughly
/// double what a well-written tool needs: a check that fires on ordinary work gets switched
/// off, and a disabled check protects nothing. It catches bloat and ignores craftsmanship.
pub const PER_TOOL_ALLOWANCE: usize = 800;

/// What the model sees for one tool (§5.4). Built once at startup; never rebuilt per request.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub struct ToolDescriptor {
    /// The tool name, as the model calls it.
    pub name: &'static str,

    /// What the tool does, and when to use it.
    pub description: &'static str,

    /// The input schema, derived from the tool's `Input` type and then minified (D17).
    pub input_schema: Arc<JsonObject>,

    /// Only the hints that differ from the specification's defaults (D16).
    pub annotations: ToolAnnotations,
}

impl ToolDescriptor {
    /// Builds a descriptor whose schema is derived from `I` and minified by the one shared
    /// pipeline (D17).
    ///
    /// # Panics
    ///
    /// Panics when `I` does not derive a root `type: "object"` schema, which the specification
    /// requires of every tool input. That is a compile-time property of the type expressed as a
    /// startup assertion, not a runtime condition.
    pub fn new<I>(
        name: &'static str,
        description: &'static str,
        annotations: ToolAnnotations,
    ) -> Self
    where
        I: schemars::JsonSchema + std::any::Any,
    {
        let derived = schema_for_input::<I>()
            .unwrap_or_else(|err| panic!("tool `{name}` has an unusable input schema: {err}"));

        Self {
            name,
            description,
            input_schema: Arc::new(minify_input_schema(derived.as_ref())),
            annotations,
        }
    }
}

impl From<&ToolDescriptor> for Tool {
    fn from(descriptor: &ToolDescriptor) -> Self {
        // `output_schema` is left unset and no `title` or `icons` are added: the SDK's
        // constructor defaults them away, and no tool emits an `outputSchema` (D15) — asserted
        // by a test rather than left to convention.
        Tool::new(
            descriptor.name,
            descriptor.description,
            descriptor.input_schema.clone(),
        )
        .with_annotations(descriptor.annotations.clone())
    }
}

/// The tool set, built **once** at startup and held in `AppState` behind an `Arc` (D4, §5.4).
///
/// There are no profiles: `tools/list` advertises the complete set to everyone (D22).
/// Authorization belongs to the policy and the engine, and advertisement is not authorization.
pub struct Registry {
    router: ToolRouter<CatalogHandler>,
    tools: Vec<Tool>,
    serialised_bytes: usize,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.tools.len())
            .field("serialised_bytes", &self.serialised_bytes)
            .finish()
    }
}

impl Registry {
    /// Builds the registry from a router.
    ///
    /// The answer to `tools/list` is prebuilt here — `list_all()` is sorted by name, which is
    /// the determinism the specification asks for, so nothing sorts twice — and serialised once
    /// so its size is a number a test can assert and a metric can report.
    pub fn new(router: ToolRouter<CatalogHandler>) -> Self {
        let tools = router.list_all();
        let serialised_bytes = serde_json::to_vec(&tools)
            .expect("a tool descriptor is always serialisable")
            .len();

        Self {
            router,
            tools,
            serialised_bytes,
        }
    }

    /// The complete tool set this server ships.
    ///
    /// Ordering is not set here: `list_all()` sorts by name, which is the determinism the
    /// specification asks for, so nothing sorts twice.
    pub fn with_shipped_tools() -> Self {
        Self::new(
            ToolRouter::new()
                .with_route(route_for(tools::hello::Hello))
                .with_route(route_for(tools::list_tenants::ListTenants)),
        )
    }

    /// The prebuilt answer to `tools/list`.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// The router, for dispatching a `tools/call`.
    pub fn router(&self) -> &ToolRouter<CatalogHandler> {
        &self.router
    }

    /// One tool by name, serving byte-identically what `tools/list` served (D17).
    pub fn tool(&self, name: &str) -> Option<Tool> {
        self.router.get(name).cloned()
    }

    /// Size of the serialised `tools/list` payload, reported by `mcp_tools_list_bytes` and
    /// asserted against [`Self::byte_budget`] in CI.
    pub fn serialised_bytes(&self) -> usize {
        self.serialised_bytes
    }

    /// The ceiling the payload must stay inside: `PER_TOOL_ALLOWANCE × tool count` (D12).
    ///
    /// Adding a tool raises the budget automatically, so functionality growing can never create
    /// pressure to cut it. Only bloat — more bytes for the same tools — fails the check.
    pub fn byte_budget(&self) -> usize {
        PER_TOOL_ALLOWANCE * self.tools.len()
    }

    /// The per-tool byte table CI prints whether the budget check passes or fails, so growth is
    /// always attributable and the review question is *"T4 grew 312 bytes, is it earning
    /// that?"* rather than *"the payload is too big"*.
    ///
    // Reached only from `cargo make budget`, which is a CI assertion on a build artifact rather
    // than a runtime behaviour (D34) — hence the allow, matching `catalog-engine`'s convention
    // for test-only-reachable items.
    #[allow(dead_code)]
    pub fn byte_table(&self) -> Vec<(String, usize)> {
        self.tools
            .iter()
            .map(|tool| {
                let bytes = serde_json::to_vec(tool)
                    .expect("a tool descriptor is always serialisable")
                    .len();

                (tool.name.to_string(), bytes)
            })
            .collect()
    }
}

/// Puts a [`CatalogTool`] behind the SDK's router — the object-safe adapter of §5.5.
///
/// It is the **only** place the steps between a JSON-RPC request and a tool happen, which is
/// what stops each tool re-deriving them:
///
/// - `Value → T::Input` with the serde path kept, so a bad argument names its own field (rule 3);
/// - the [`CallContext`], built from the request's identity on every call and never cached on
///   the handler (D4);
/// - rendering, so a tool cannot set `isError`, and the engine's warnings reach the model
///   whether or not the tool looked at them (rule 2, D28).
pub fn route_for<T: CatalogTool>(tool: T) -> ToolRoute<CatalogHandler> {
    let descriptor = T::descriptor();
    let attributes: Tool = (&descriptor).into();
    let tool = Arc::new(tool);

    ToolRoute::new_dyn(
        attributes,
        move |context: rmcp::handler::server::tool::ToolCallContext<'_, CatalogHandler>| {
            let tool = tool.clone();

            Box::pin(async move {
                let state = context.service.state();
                let request = context.request_context();

                let identity = Arc::new(caller_identity(request));
                let call = CallContext::new(
                    state.engine_for(identity.clone()),
                    state.deadline(),
                    request.ct.clone(),
                    request
                        .meta
                        .get_progress_token()
                        .map(|token| ProgressSink::new(request.peer.clone(), token)),
                    identity.tenant_key(),
                );

                // Rule 3 — an argument failure is a **tool** error the model can correct, not a
                // protocol error it cannot see.
                let arguments = serde_json::Value::Object(context.arguments.unwrap_or_default());
                let input = match serde_path_to_error::deserialize::<_, T::Input>(arguments) {
                    Ok(input) => input,
                    Err(err) => {
                        return Ok(crate::handler::tool_error_result(&invalid_arguments(&err)));
                    }
                };

                // Rule 2 — a tool cannot set `isError`; returning `Err` is how it fails. The
                // engine warnings are read off the call's client **here**, after the tool, so
                // what reaches the model does not depend on the tool remembering them (D28).
                match tool.call(&call, input).await {
                    Ok(output) => {
                        let engine_warnings = call.engine().call_warnings().collected();

                        Ok(CallToolResponse::from(success_result(
                            &output,
                            engine_warnings.as_deref(),
                        )))
                    }
                    Err(error) => Ok(crate::handler::tool_error_result(&error)),
                }
            })
        },
    )
}

#[cfg(test)]
mod tests;
