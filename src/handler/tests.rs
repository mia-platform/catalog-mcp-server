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
    context::AppState,
    handler::{CatalogHandler, MAX_INSTRUCTIONS_BYTES},
};
use configuration::Config;
use rmcp::ServerHandler;
use rstest::{fixture, rstest};

#[fixture]
fn mock_handler() -> CatalogHandler {
    let mut config = Config::default();

    config.server.allowed_hosts = vec!["catalog-mcp.example.com".to_string()];
    config.engine.base_url = "http://api-gateway:8080".to_string();
    config.auth.resource = "https://catalog-mcp.example.com/mcp".to_string();

    CatalogHandler::new(AppState::new(config).expect("a valid state"))
}

/// D5 — capabilities are `tools` only. No `resources`, `prompts`, `completions` or `logging`,
/// therefore no `subscriptions/listen` and no `notifications/tools/list_changed`.
#[rstest]
fn test_capabilities_are_tools_only(mock_handler: CatalogHandler) {
    let capabilities = mock_handler.get_info().capabilities;

    assert!(capabilities.tools.is_some());
    assert!(capabilities.resources.is_none());
    assert!(capabilities.prompts.is_none());
    assert!(capabilities.completions.is_none());
}

/// D5 — `listChanged` is left **absent**, which is what "not supported" means on the wire.
/// The explicit `false` is unreachable through the builder and buys nothing.
#[rstest]
fn test_list_changed_is_absent(mock_handler: CatalogHandler) {
    let serialised = serde_json::to_value(mock_handler.get_info().capabilities)
        .expect("capabilities are serialisable");

    assert_eq!(serialised["tools"], serde_json::json!({}));
}

/// §5.3 — name and version come from the build environment, as the previous server did.
#[rstest]
fn test_server_identity_comes_from_the_build_environment(mock_handler: CatalogHandler) {
    let info = mock_handler.get_info();

    assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
}

/// D14 — `instructions` says only what no tool description can, and is capped at 400 bytes.
/// Part 4's rule 2 (every tool usable with no system-prompt instructions) makes anything longer
/// a smell rather than a feature.
#[rstest]
fn test_instructions_are_within_their_cap(mock_handler: CatalogHandler) {
    let instructions = mock_handler
        .get_info()
        .instructions
        .expect("the server ships instructions");

    assert!(
        instructions.len() <= MAX_INSTRUCTIONS_BYTES,
        "instructions are {} bytes, over the {MAX_INSTRUCTIONS_BYTES} byte cap",
        instructions.len()
    );
    assert!(instructions.contains("multi-tenant"));
}

/// D3 — the advertised version set is read from the SDK, never hardcoded, so an SDK upgrade
/// cannot leave us advertising a revision we no longer serve.
#[rstest]
fn test_supported_versions_are_the_sdks_own(mock_handler: CatalogHandler) {
    assert_eq!(
        ServerHandler::supported_protocol_versions(&mock_handler).as_ref(),
        rmcp::model::ProtocolVersion::KNOWN_VERSIONS
    );
}

/// D17 — `get_tool` answers with the registry's schema, and `None` for a name nobody
/// registered. The unknown-tool *call* path is the SDK router's, not ours.
#[rstest]
fn test_get_tool_answers_for_registered_names_only(mock_handler: CatalogHandler) {
    assert!(
        mock_handler
            .get_tool(crate::tools::hello::TOOL_NAME)
            .is_some()
    );
    assert!(mock_handler.get_tool("no-such-tool").is_none());
}
