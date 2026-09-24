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
use crate::{context::AppState, server::identity::MiaIdentity};
use catalog_client::CallerIdentity;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::tool::ToolCallContext,
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CompleteRequestMethod,
        CompleteRequestParams, CompleteResult, DiscoverResult, ListPromptsRequestMethod,
        ListPromptsResult, ListResourceTemplatesRequestMethod, ListResourceTemplatesResult,
        ListResourcesRequestMethod, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
    },
    service::{RequestContext, RoleServer},
};
use std::borrow::Cow;

/// What the server tells the model that no tool description can (D14).
///
/// Capped at [`MAX_INSTRUCTIONS_BYTES`] and asserted by a test: Part 4's rule 2 — every tool
/// usable with no system-prompt instructions — makes anything longer a smell, not a feature.
const INSTRUCTIONS: &str = "This catalog is multi-tenant. The tenant you are acting in is fixed \
                            by the request and cannot be chosen, changed or widened from a tool \
                            argument. Every result is scoped to it.";

/// Ceiling on [`INSTRUCTIONS`], in bytes (D14).
pub const MAX_INSTRUCTIONS_BYTES: usize = 400;

// D14 as a compile error rather than a test failure: the cap cannot be exceeded by a commit
// that forgets to run the tests.
const _: () = assert!(
    INSTRUCTIONS.len() <= MAX_INSTRUCTIONS_BYTES,
    "`instructions` exceeds its 400-byte cap (D14)"
);

/// The MCP protocol revision from which cache hints and `resultType` are required (SEP-2549,
/// SEP-2322). Below it they are omitted, exactly as the SDK's own macro gates them.
const CACHE_HINT_FLOOR: ProtocolVersion = ProtocolVersion::V_2026_07_28;

/// Our half of the wire (§5.1, D6).
///
/// The handler implements four things — server identity, `list_tools`, `get_tool`, `call_tool` —
/// plus a thin `server/discover` override that does nothing but attach the cache hints of D13.
/// Everything else is the SDK's: framing, era negotiation, `initialize`, `ping`, header↔body
/// validation, status codes and SSE plumbing.
///
/// **It holds no per-request state, ever** (D4). Its lifetime is not uniform — one instance per
/// session in legacy mode, one per request when stateless, plus extras to populate the SDK's
/// schema cache — so a field here would be a cross-request leak rather than a cache. Everything
/// shared lives in [`AppState`] behind an `Arc`, and the factory clones `Arc`s and nothing else.
#[derive(Clone, Debug)]
pub struct CatalogHandler {
    state: AppState,
}

impl CatalogHandler {
    /// Builds a handler over the shared state. Cheap by contract: the transport calls the
    /// factory per session, per request in stateless mode, and once per tool name.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// The shared state behind this handler.
    ///
    // Its production caller is the `CallContext` of §5.5, which Step 4 freezes; until then it is
    // reached only from the tenant-isolation test's probe tool. Hence the allow.
    #[allow(dead_code)]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Whether the negotiated revision requires the SEP-2549 cache hints.
    ///
    /// Mirrors the SDK's own gate rather than reimplementing a version policy: an absent
    /// version is a legacy peer.
    fn wants_cache_hints(context: &RequestContext<RoleServer>) -> bool {
        context
            .protocol_version()
            .is_some_and(|version| version.as_str() >= CACHE_HINT_FLOOR.as_str())
    }
}

/// The inbound HTTP request parts the transport injected into the request context (§6.2).
///
/// This is the **one** place the dig happens. The `Parts` ride on the *message*, not on the
/// handler, which is what makes it correct in legacy mode where one handler instance serves a
/// whole session (D4). `None` on any transport that is not HTTP.
pub fn inbound_parts(context: &RequestContext<RoleServer>) -> Option<&http::request::Parts> {
    context.extensions.get::<http::request::Parts>()
}

/// The caller's identity for this request (§6.2).
///
/// Two digs, because the value the tower layer inserted sits one level deeper than the `Parts`
/// the transport injected: `RequestContext.extensions` holds the `Parts`, and the layer's
/// `MiaIdentity` is inside `Parts.extensions`.
///
/// **It is read on every call and never cached on the handler** (D4): in legacy mode one handler
/// instance serves a whole session, so a field here would be a cross-request leak. An absent
/// identity yields an empty one rather than an error — the server relays identity, it does not
/// adjudicate it (D47).
pub fn caller_identity(context: &RequestContext<RoleServer>) -> CallerIdentity {
    inbound_parts(context)
        .and_then(|parts| parts.extensions.get::<MiaIdentity>())
        .map(CallerIdentity::from)
        .unwrap_or_default()
}

impl ServerHandler for CatalogHandler {
    /// Server identity, used by both `server/discover` and `initialize`, so the two eras cannot
    /// describe different servers (§5.3).
    ///
    /// Capabilities are `tools` only (D5). `listChanged` is left **absent**, which is what "not
    /// supported" means on the wire and what the builder produces; tool-set invalidation is the
    /// cache hint, not a push.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
            .with_server_info(rmcp::model::Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
    }

    /// We do not restrict the advertised protocol versions (D3).
    ///
    /// Whatever `rmcp` supports is what we support: the set is read from the SDK, never
    /// hardcoded, so an SDK upgrade cannot leave us advertising a revision we no longer serve.
    /// This is the SDK's own default, restated so the decision is visible where it applies.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS)
    }

    /// `server/discover` with the real cache hints (D13).
    ///
    /// The SDK derives the whole payload from `get_info()` and `supported_protocol_versions()`;
    /// the only thing wrong with its default is `ttlMs: 0`, so that is the only thing we touch.
    fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<DiscoverResult, McpError>> + Send + '_ {
        let mut result = DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        );

        result.ttl_ms = self.state.config.tools.tools_list_ttl_ms;
        result.cache_scope = CacheScope::Private;

        std::future::ready(Ok(result))
    }

    /// The prebuilt, byte-budgeted `tools/list` (D12, D13).
    ///
    /// Hand-written rather than macro-generated for two reasons, of which the second is the
    /// larger: the macro answers `cacheScope: Public` with `ttlMs: 0`, and the payload itself is
    /// the number this rewrite exists to move. The answer is a clone of a vector built once at
    /// startup; `cursor` is ignored, as the SDK's own macro does, because the set is not
    /// paginated.
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let mut result = ListToolsResult::with_all_items(self.state.registry.tools().to_vec());

        // `"private"` is kept even though the set is identical for every caller (D22), because
        // `"public"` licenses an intermediary to share the response between callers even when it
        // came from an authenticated endpoint — a one-way door, for no measurable saving on a
        // payload this size.
        if Self::wants_cache_hints(&context) {
            result = result
                .with_ttl_ms(self.state.config.tools.tools_list_ttl_ms)
                .with_cache_scope(CacheScope::Private);
        }

        std::future::ready(Ok(result))
    }

    /// The same normalised schema `list_tools` served (D17).
    ///
    /// This is ours **only** because the SDK's `Mcp-Param-*` validation reads it: two sources
    /// for one schema would diverge silently.
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.state.registry.tool(name)
    }

    /// D5 — capabilities are `tools` only, and these four make that true **on the wire**.
    ///
    /// The SDK's default `ServerHandler` answers `prompts/list`, `resources/list`,
    /// `resources/templates/list` and `completion/complete` with an empty success rather than
    /// `-32601`, so a server that declares only `tools` still looks as though it handles them.
    /// The conformance suite calls that out — *"server handles prompts but did not declare
    /// prompts capability"* — and it is right: a capability we do not advertise must not answer.
    /// Every other unsupported method already errors by default.
    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        std::future::ready(Err(McpError::method_not_found::<ListPromptsRequestMethod>()))
    }

    /// See [`Self::list_prompts`]: an undeclared capability must not answer.
    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        std::future::ready(Err(
            McpError::method_not_found::<ListResourcesRequestMethod>(),
        ))
    }

    /// See [`Self::list_prompts`]: an undeclared capability must not answer.
    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        std::future::ready(Err(McpError::method_not_found::<
            ListResourceTemplatesRequestMethod,
        >()))
    }

    /// See [`Self::list_prompts`]: an undeclared capability must not answer.
    fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        std::future::ready(Err(McpError::method_not_found::<CompleteRequestMethod>()))
    }

    /// Dispatches one tool call.
    ///
    /// Identity is read from the request context on every call, never from a handler field
    /// (D4). Unknown-tool is the SDK router's path and we never implement it.
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        // §7.2 — decode the ACL context for the tenant log and metric fields, and carry on
        // whatever it says. The principal id is recorded as received and never required: it is
        // an actor id rather than a secret, and it is the field that makes an audit trail
        // followable. The raw ACL context and the bearer are never logged.
        let identity = caller_identity(&context);
        tracing::debug!(
            tenant = %identity.tenant_key(),
            principal_id = identity.principal_id().unwrap_or("none"),
            tool = %request.name,
            "tool call"
        );

        let context = ToolCallContext::new(self, request, context);

        async move { self.state.registry.router().call(context).await }
    }
}

#[cfg(test)]
mod tests;
