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
    Config, ENGINE_SERVICE_LABEL, auth::AuthMode, error::ConfigError, load, server::ServerConfig,
};
use rstest::{fixture, rstest};

/// A configuration that passes every rule in [`Config::validate`], for a test to break one
/// thing at a time.
#[fixture]
fn mock_valid_config() -> Config {
    Config {
        server: ServerConfig {
            allowed_hosts: vec!["catalog-mcp.example.com".to_string()],
            ..ServerConfig::default()
        },
        ..Config::default()
    }
    .with_engine_base_url("http://api-gateway:8080")
    .with_auth_resource("https://catalog-mcp.example.com/mcp")
}

impl Config {
    /// Test-only builder step keeping the fixture readable.
    fn with_engine_base_url(mut self, base_url: &str) -> Self {
        self.engine.base_url = base_url.to_string();
        self
    }

    /// Test-only builder step keeping the fixture readable.
    fn with_auth_resource(mut self, resource: &str) -> Self {
        self.auth.resource = resource.to_string();
        self
    }
}

/// The field path and the reason of an `Invalid`, or a panic naming what came instead.
fn invalid_parts(error: ConfigError) -> (&'static str, String) {
    match error {
        ConfigError::Invalid { field, reason } => (field, reason),
        other => panic!("expected a validation refusal, got: {other}"),
    }
}

#[rstest]
fn test_valid_configuration_is_accepted(mock_valid_config: Config) {
    assert!(mock_valid_config.validate().is_ok());
}

/// D11 — the first-deploy failure this plan most expects. The message has to name the symptom,
/// because `403 Forbidden: Host header is not allowed` reads as a routing bug.
#[rstest]
fn test_empty_allowed_hosts_is_refused(mock_valid_config: Config) {
    let mut config = mock_valid_config;
    config.server.allowed_hosts.clear();

    let (field, reason) = invalid_parts(config.validate().expect_err("empty allowedHosts"));

    assert_eq!(field, "server.allowedHosts");
    assert!(reason.contains("403 Forbidden: Host header is not allowed"));
}

/// D46 — a config value for an unimplemented mode is a startup failure, not a surprise at
/// request time.
#[rstest]
fn test_resource_server_auth_mode_is_refused(mock_valid_config: Config) {
    let mut config = mock_valid_config;
    config.auth.mode = AuthMode::ResourceServer;

    let (field, reason) = invalid_parts(config.validate().expect_err("resource-server mode"));

    assert_eq!(field, "auth.mode");
    assert!(reason.contains("not implemented"));
    assert!(reason.contains("P-C3"));
}

#[rstest]
#[case::empty("")]
#[case::fragment("https://catalog-mcp.example.com/mcp#section")]
#[case::trailing_slash("https://catalog-mcp.example.com/mcp/")]
#[case::relative("/mcp")]
#[case::no_scheme("catalog-mcp.example.com/mcp")]
fn test_non_canonical_auth_resource_is_refused(mock_valid_config: Config, #[case] resource: &str) {
    let config = mock_valid_config.with_auth_resource(resource);

    let (field, _) = invalid_parts(config.validate().expect_err("non-canonical auth.resource"));

    assert_eq!(field, "auth.resource");
}

/// D27 and D48 — addressing the engine `Service` directly skips `ext_authz`, which is an
/// authorization bypass that is invisible in testing.
#[rstest]
#[case::bare("http://catalog-engine")]
#[case::with_port("http://catalog-engine:80")]
#[case::fully_qualified("http://catalog-engine.my-namespace.svc.cluster.local")]
#[case::mixed_case("http://Catalog-Engine:80")]
fn test_engine_service_base_url_is_refused(mock_valid_config: Config, #[case] base_url: &str) {
    let config = mock_valid_config.with_engine_base_url(base_url);

    let (field, reason) = invalid_parts(config.validate().expect_err("engine Service base URL"));

    assert_eq!(field, "engine.baseUrl");
    assert!(reason.contains(ENGINE_SERVICE_LABEL));
}

#[rstest]
#[case::empty("")]
#[case::not_a_url("api-gateway:8080/x y")]
#[case::relative("/api/catalog")]
fn test_non_absolute_engine_base_url_is_refused(mock_valid_config: Config, #[case] base_url: &str) {
    let config = mock_valid_config.with_engine_base_url(base_url);

    let (field, _) = invalid_parts(config.validate().expect_err("non-absolute engine.baseUrl"));

    assert_eq!(field, "engine.baseUrl");
}

/// §11 — a deadline below one engine hop makes every call time out at the wrong layer.
#[rstest]
fn test_call_deadline_below_engine_timeouts_is_refused(mock_valid_config: Config) {
    let mut config = mock_valid_config;
    config.engine.timeout_ms = 5_000;
    config.engine.connect_timeout_ms = 1_000;
    config.tools.call_deadline_seconds = 5;

    let (field, reason) = invalid_parts(config.validate().expect_err("deadline below one hop"));

    assert_eq!(field, "tools.callDeadlineSeconds");
    assert!(reason.contains("6000 ms"));
}

#[rstest]
fn test_call_deadline_equal_to_engine_timeouts_is_accepted(mock_valid_config: Config) {
    let mut config = mock_valid_config;
    config.engine.timeout_ms = 5_000;
    config.engine.connect_timeout_ms = 1_000;
    config.tools.call_deadline_seconds = 6;

    assert!(config.validate().is_ok());
}

/// Per-field renames, not a blanket `rename_all`: the wire names are what the chart writes.
#[rstest]
fn test_camel_case_field_names_deserialise() {
    let raw = br#"{
      "server":  { "ip": "0.0.0.0", "port": 8000, "mcpPath": "/mcp",
                   "allowedHosts": ["catalog-mcp.example.com"], "allowedOrigins": [],
                   "maxBodyBytes": 1048576 },
      "engine":  { "baseUrl": "http://api-gateway:8080", "apiPrefix": "/api/catalog",
                   "timeoutMs": 5000, "connectTimeoutMs": 1000, "maxRetries": 1 },
      "auth":    { "mode": "gateway", "resource": "https://catalog-mcp.example.com/mcp",
                   "resourceMetadataUrl":
                     "https://catalog-mcp.example.com/.well-known/oauth-protected-resource/mcp" },
      "tools":   { "callDeadlineSeconds": 25, "toolsListTtlMs": 3600000,
                   "rateLimit": { "enabled": true, "perTenantCallsPerMinute": 120, "burst": 20 },
                   "defaults": { "searchLimit": 50, "relationshipLimit": 50, "historyLimit": 10,
                                 "auditLimit": 25, "complianceWaitSeconds": 20,
                                 "maxWriteBytes": 262144 } },
      "response":{ "structuredContent": false },
      "transport":{ "legacySessionMode": true, "jsonResponse": true, "sseKeepAliveSeconds": 15 },
      "health":  { "readinessChecksEngine": true, "engineProbeTimeoutMs": 2000 },
      "observability": { "metricsEnabled": true }
    }"#;

    let config: Config = serde_json::from_slice(raw).expect("the §11 configuration deserialises");

    assert!(config.validate().is_ok());
    assert_eq!(config.server.mcp_path, "/mcp");
    assert_eq!(config.engine.api_prefix, "/api/catalog");
    assert_eq!(config.auth.mode, AuthMode::Gateway);
    assert_eq!(config.tools.call_deadline_seconds, 25);
    assert!(!config.response.structured_content);
    assert!(config.transport.legacy_session_mode);
    assert!(config.health.readiness_checks_engine);
    assert!(config.observability.metrics_enabled);
}

/// The defaults of every nested struct, so an omitted block is the documented value rather
/// than a zero.
#[rstest]
fn test_omitted_blocks_take_their_defaults() {
    let config: Config = serde_json::from_slice(b"{}").expect("an empty object deserialises");

    assert_eq!(config.server.port, 8000);
    assert_eq!(config.server.max_body_bytes, 1_048_576);
    assert_eq!(config.engine.timeout_ms, 5_000);
    assert_eq!(config.tools.rate_limit.per_tenant_calls_per_minute, 120);
    assert_eq!(config.tools.defaults.max_write_bytes, 262_144);
    assert_eq!(config.transport.sse_keep_alive_seconds, 15);
    // ...and it is still refused, because `allowedHosts` has no usable default (D11).
    assert!(config.validate().is_err());
}

#[rstest]
fn test_load_reports_the_missing_file_path() {
    let folder = std::path::Path::new("/nonexistent/catalog-mcp-server-test");

    let error = load(folder).expect_err("a missing configuration folder");

    assert!(matches!(error, ConfigError::Read { .. }));
    assert!(error.to_string().contains("config.json"));
}
