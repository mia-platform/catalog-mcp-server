/**
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
use http::request::Parts;
use reqwest::header::{HeaderMap, HeaderName};
use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ErrorData, InitializeResult, ListToolsResult,
        PaginatedRequestParams,
    },
    service::{RequestContext, RoleServer},
};
use rmcp_openapi::{HttpClient, Server as McpServer, Tool, ToolCollection};
use url::Url;

/// Inbound MCP request headers that are proxied verbatim to the upstream Catalog
/// service on every tool call. Lower-case, as required by `HeaderName::from_static`.
const PROXIED_HEADERS: &[&str] = &["x-mia-acl-context"];

/// A [`ServerHandler`] that wraps the OpenAPI-backed [`McpServer`] and forwards a
/// fixed allow-list of inbound HTTP headers ([`PROXIED_HEADERS`]) to the upstream
/// service invoked by each tool.
///
/// `rmcp-openapi` bakes the outbound HTTP headers into every tool at build time and
/// only exposes per-request forwarding for the `Authorization` header (behind a
/// feature flag, and only when driven by the `rmcp-actix-web` transport). Since this
/// server runs on the axum `StreamableHttpService` transport, we instead read the
/// full inbound request from `context.extensions` — the transport injects the
/// `http::request::Parts` there — and rebuild a per-request server whose tools carry
/// the proxied headers as default headers.
#[derive(Clone)]
pub struct HeaderProxyServer {
    inner: McpServer,
    base_url: Url,
}

impl HeaderProxyServer {
    pub fn new(inner: McpServer, base_url: Url) -> Self {
        Self { inner, base_url }
    }

    /// Collect the allow-listed headers from the inbound request parts.
    fn collect_proxied_headers(parts: Option<&Parts>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let Some(parts) = parts else {
            return headers;
        };

        for name in PROXIED_HEADERS {
            if let Some(value) = parts.headers.get(*name) {
                headers.insert(HeaderName::from_static(name), value.clone());
            }
        }

        headers
    }

    /// Build a per-request clone of the inner server whose tools send the proxied
    /// headers upstream. Everything else (filters, transformers, metadata) is
    /// preserved from the pre-built server.
    fn per_request_server(&self, proxied: HeaderMap) -> Result<McpServer, ErrorData> {
        // Merge the server's static default headers (if any) with the proxied ones;
        // proxied headers win on conflict.
        let mut headers = self.inner.default_headers.clone().unwrap_or_default();
        headers.extend(proxied);

        // One HTTP client (one reqwest connection pool) shared across all tools via
        // cheap `Clone` — mirrors how `rmcp-openapi` builds tool clients internally.
        let http_client = HttpClient::new()
            .with_base_url(self.base_url.clone())
            .map_err(|err| {
                ErrorData::internal_error(format!("cannot build upstream HTTP client: {err}"), None)
            })?
            .with_default_headers(headers);

        let tools = self
            .inner
            .tool_collection
            .iter()
            .map(|tool| Tool::new(tool.metadata.clone(), http_client.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                ErrorData::internal_error(format!("cannot rebuild tool for request: {err}"), None)
            })?;

        let mut server = self.inner.clone();
        server.tool_collection = ToolCollection::from_tools(tools);
        Ok(server)
    }
}

impl ServerHandler for HeaderProxyServer {
    fn get_info(&self) -> InitializeResult {
        self.inner.get_info()
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.inner.list_tools(request, context).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        // The axum streamable-http transport injects the inbound request parts into
        // the context extensions; extract the allow-listed headers before `context`
        // is moved into the delegated call.
        let proxied = Self::collect_proxied_headers(context.extensions.get::<Parts>());
        let server = self.per_request_server(proxied)?;
        server.call_tool(request, context).await
    }
}
