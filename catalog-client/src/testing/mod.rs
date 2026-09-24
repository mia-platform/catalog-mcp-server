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
// The mock engine (§12.5).
//
// Behind the `testing` feature so the binary crate's own tests can use it too: built once, used
// by every wave. If it were not funded here, each tool plan would rebuild a worse one.
//
// Fixtures are **engine-shaped by construction** — the list envelope, the error body, the
// `Warning` header and the `PartialObjectMetadata` projection are built from the shapes
// `protocol-findings §4` records, not invented — so a fixture cannot drift into something the
// engine never produces.

use crate::{
    client::{Deadline, EngineClient, EngineClientFactory},
    identity::CallerIdentity,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// A fictional organization, per D39: fictional identifiers only.
pub const MOCK_ORGANIZATION: &str = "my-org";

/// A fictional tenant.
pub const MOCK_TENANT: &str = "my-tenant";

/// A fictional item name.
pub const MOCK_ITEM_NAME: &str = "example-item";

/// A fictional principal id.
pub const MOCK_PRINCIPAL_ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

/// A fictional bearer.
pub const MOCK_BEARER: &str = "Bearer test-token";

/// A fictional request id.
pub const MOCK_REQUEST_ID: &str = "test-request-0001";

/// The base64url-unpadded ACL context the policy layer would have emitted for the fixtures.
pub fn mock_acl_context() -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    URL_SAFE_NO_PAD
        .encode(json!({ "organization": MOCK_ORGANIZATION, "tenant": MOCK_TENANT }).to_string())
}

/// The identity a request carries when everything arrived.
pub fn mock_identity() -> Arc<CallerIdentity> {
    Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        Some(MOCK_PRINCIPAL_ID),
        Some(MOCK_BEARER),
        Some(MOCK_REQUEST_ID),
    ))
}

/// The identity a request carries on the in-cluster path, where Envoy is bypassed: an ACL
/// context and nothing else.
pub fn mock_identity_acl_only() -> Arc<CallerIdentity> {
    Arc::new(CallerIdentity::new(
        Some(&mock_acl_context()),
        None,
        None,
        None,
    ))
}

/// The identity a request carries when **nothing** arrived, which D47 says must still reach the
/// engine rather than be refused here.
pub fn mock_identity_empty() -> Arc<CallerIdentity> {
    Arc::new(CallerIdentity::default())
}

/// A mock engine, ready to be given expectations.
pub struct MockEngine {
    server: MockServer,
}

impl MockEngine {
    /// Starts a mock engine on a free port.
    pub async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    /// The `MockServer`, for expectations this helper does not cover.
    pub fn server(&self) -> &MockServer {
        &self.server
    }

    /// A client bound to the given identity, with a generous deadline.
    ///
    /// The API prefix is `/` rather than `/api/catalog`: the mock stands in for the engine
    /// behind the gateway, and the prefix is the gateway's business.
    pub fn client(&self, identity: Arc<CallerIdentity>) -> EngineClient {
        self.client_with_deadline(identity, Duration::from_secs(25))
    }

    /// A client whose whole-call budget is `budget`.
    pub fn client_with_deadline(
        &self,
        identity: Arc<CallerIdentity>,
        budget: Duration,
    ) -> EngineClient {
        EngineClientFactory::new(
            &self.server.uri(),
            "/",
            Duration::from_secs(5),
            Duration::from_secs(1),
            1,
        )
        .expect("the mock engine URL is well formed")
        .bind(identity, Deadline::starting_now(budget))
    }

    /// A client that never retries, for asserting a single attempt.
    pub fn client_without_retries(&self, identity: Arc<CallerIdentity>) -> EngineClient {
        EngineClientFactory::new(
            &self.server.uri(),
            "/",
            Duration::from_secs(5),
            Duration::from_secs(1),
            0,
        )
        .expect("the mock engine URL is well formed")
        .bind(identity, Deadline::starting_now(Duration::from_secs(25)))
    }

    /// Answers `GET <path>` with `200` and the given body.
    pub async fn get_ok(&self, url_path: &str, body: Value) {
        self.mount(url_path, ResponseTemplate::new(200).set_body_json(body))
            .await;
    }

    /// Answers `GET <path>` with `200`, the given body and one `Warning` header per message.
    pub async fn get_ok_with_warnings(&self, url_path: &str, body: Value, warnings: &[&str]) {
        let mut template = ResponseTemplate::new(200).set_body_json(body);

        for warning in warnings {
            template = template.append_header("Warning", format!(r#"299 - "{warning}""#).as_str());
        }

        self.mount(url_path, template).await;
    }

    /// Answers `GET <path>` with the engine's own error envelope.
    pub async fn get_error(&self, url_path: &str, status: u16, message: &str) {
        self.mount(
            url_path,
            ResponseTemplate::new(status)
                .set_body_json(mock_error_body(status, message))
                .append_header("x-request-id", "engine-request-0001"),
        )
        .await;
    }

    /// Mounts one expectation.
    async fn mount(&self, url_path: &str, template: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(url_path))
            .respond_with(template)
            .mount(&self.server)
            .await;
    }
}

/// The engine's error body: `{"status", "error", "message"}` — not RFC 7807.
pub fn mock_error_body(status: u16, message: &str) -> Value {
    json!({
        "status": status,
        "error": canonical_reason(status),
        "message": message,
    })
}

/// The engine's list envelope. `metadata.continue` is **omitted** on the last page, which is how
/// end-of-results is signalled.
pub fn mock_list_envelope(items: Vec<Value>, continue_token: Option<&str>) -> Value {
    let mut metadata = serde_json::Map::new();

    if let Some(token) = continue_token {
        metadata.insert("continue".to_string(), json!(token));
    }

    json!({
        "apiVersion": "mia-platform.eu/v1",
        "kind": "List",
        "metadata": Value::Object(metadata),
        "items": items,
    })
}

/// A realistic item, shaped as the engine's own manifests are.
pub fn mock_item(name: &str) -> Value {
    json!({
        "apiVersion": "stable.example.com/v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "family": "services",
            "title": "Example Service",
            "description": "A service used in tests.",
            "labels": { "environment": "demo" },
            "tags": ["api"],
            "urn": format!("urn:mia-platform-catalog:stable.example.com:v1:Service:{name}"),
            "uid": "550e8400-e29b-41d4-a716-446655440000",
            "creationTimestamp": "2026-09-17T10:30:45Z",
            "updateTimestamp": "2026-09-17T10:30:45Z",
        },
        "spec": { "replicas": 2 },
        "resourceVersion": "1",
    })
}

/// A realistic Item Type Definition, shaped as the engine's own manifests are.
pub fn mock_item_type_definition(kind: &str, plural: &str, group: &str) -> Value {
    json!({
        "apiVersion": "mia-platform.eu/v1",
        "kind": "ItemTypeDefinition",
        "metadata": { "name": format!("{plural}.{group}") },
        "spec": {
            "group": group,
            "names": {
                "kind": kind,
                "plural": plural,
                "singular": plural.trim_end_matches('s'),
                "displayPlural": "Example services",
                "displaySingular": "Example service",
            },
            "scope": "Tenant",
            "versions": [{
                "name": "v1",
                "served": true,
                "deprecated": false,
                "schema": { "openAPIV31Schema": { "type": "object", "properties": {} } },
                "selectableFields": [{ "jsonPath": "spec.replicas" }],
            }],
            "history": { "enabled": true },
        },
        "resourceVersion": "1",
    })
}

/// The canonical reason phrase the engine puts in `error`.
fn canonical_reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}
