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
use axum::{extract::Request, middleware::Next, response::Response};
use catalog_client::{
    CallerIdentity, Sensitive,
    identity::{
        ACL_CONTEXT_HEADER, AUTHORIZATION_HEADER, AclContext, PRINCIPAL_ID_HEADER,
        REQUEST_ID_HEADER,
    },
};
use configuration::auth::{AuthConfig, AuthMode};

/// What the layer **observed** on the inbound request (§6.2).
///
/// `MiaIdentity` and [`CallerIdentity`] are two stages of one thing, and the split is
/// deliberate: this is what arrived, it knows nothing about the engine, and it is **infallible**
/// — mirroring `catalog-engine`'s own `PrincipalIdHeader`, whose extractor is
/// `Rejection = Infallible` for the same reason. `CallerIdentity` is what the client *uses*.
///
/// The raw header strings are kept alongside the decoded context because the headers are
/// forwarded **verbatim** and are never re-encoded from a parsed value — re-encoding a header we
/// do not own is how a `tenantName` gets dropped.
#[derive(Clone, Debug, Default)]
pub struct MiaIdentity {
    /// The `x-mia-acl-context` header exactly as received.
    pub acl_context: Option<String>,

    /// The decoded context, when it was there and usable. **For logging and tenancy only.**
    pub acl: Option<AclContext>,

    /// The `x-mia-principal-id` header exactly as received.
    pub principal_id: Option<String>,

    /// The caller's bearer, wrapped so it cannot be logged (D45).
    pub bearer: Option<Sensitive<String>>,

    /// The `x-request-id` header, reused if present and minted by the layer above if not.
    pub request_id: Option<String>,
}

impl MiaIdentity {
    /// Reads the D26 allowlist off an inbound request. **Never fails, never rejects.**
    fn observe(request: &Request) -> Self {
        let header = |name: &http::HeaderName| -> Option<String> {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };

        let acl_context = header(&ACL_CONTEXT_HEADER);

        Self {
            acl: acl_context.as_deref().and_then(AclContext::decode),
            acl_context,
            principal_id: header(&PRINCIPAL_ID_HEADER),
            bearer: header(&AUTHORIZATION_HEADER).map(Sensitive::new),
            request_id: header(&REQUEST_ID_HEADER),
        }
    }
}

impl From<&MiaIdentity> for CallerIdentity {
    /// One direction, no validation in either (§6.2).
    fn from(observed: &MiaIdentity) -> Self {
        CallerIdentity::new(
            observed.acl_context.as_deref(),
            observed.principal_id.as_deref(),
            observed
                .bearer
                .as_ref()
                .map(|bearer| bearer.read().as_str()),
            observed.request_id.as_deref(),
        )
    }
}

/// Reads the forwarded headers once, before the MCP service sees the request, and puts a typed
/// [`MiaIdentity`] in the request's extensions (§6.2).
///
/// **It is an extractor, not a gate** (D47). It never returns a `401`, and an absent or
/// malformed ACL context yields `acl: None` rather than a rejection. Doing it in a layer is
/// still the right place — it keeps header handling out of the JSON-RPC path — but the only
/// thing it decides is what to put in `Extensions`.
///
/// The value lands in the request's extensions, which the transport carries into the handler
/// inside `http::request::Parts`; the handler digs it out from there.
pub async fn identity_middleware(auth: AuthConfig, mut request: Request, next: Next) -> Response {
    let identity = MiaIdentity::observe(&request);

    match auth.mode {
        // Extract, never reject. The request still goes to the engine, which fails it
        // authoritatively, and on a gateway-routed hop the policy has already replaced the
        // value anyway.
        AuthMode::Gateway => {}

        // Unreachable: config validation refuses `resource-server` before the listener binds
        // (D46). The arm exists so that adding the mode is additive rather than a rewrite, and
        // so that a future contributor sees exactly where the validation branch goes.
        AuthMode::ResourceServer => {
            tracing::error!(
                "auth.mode `resource-server` reached the identity layer, which startup \
                 validation should have made impossible"
            );
        }
    }

    tracing::Span::current().record(
        "organization",
        identity
            .acl
            .as_ref()
            .map(|acl| acl.organization.as_str())
            .unwrap_or(catalog_client::identity::UNKNOWN_TENANT),
    );
    tracing::Span::current().record(
        "tenant",
        identity
            .acl
            .as_ref()
            .map(|acl| acl.tenant.as_str())
            .unwrap_or(catalog_client::identity::UNKNOWN_TENANT),
    );

    request.extensions_mut().insert(identity);

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use rstest::rstest;

    /// Builds a request carrying the given headers.
    fn mock_request(headers: &[(&str, &str)]) -> Request {
        let mut builder = HttpRequest::builder().uri("/mcp");

        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }

        builder
            .body(axum::body::Body::empty())
            .expect("a well-formed test request")
    }

    fn mock_acl_context() -> String {
        URL_SAFE_NO_PAD.encode(r#"{"organization":"my-org","tenant":"my-tenant"}"#)
    }

    #[rstest]
    fn test_the_allowlist_is_observed() {
        let acl = mock_acl_context();
        let request = mock_request(&[
            ("x-mia-acl-context", &acl),
            ("x-mia-principal-id", "3fa85f64-5717-4562-b3fc-2c963f66afa6"),
            ("authorization", "Bearer test-token"),
            ("x-request-id", "test-request-0001"),
        ]);

        let identity = MiaIdentity::observe(&request);

        assert_eq!(identity.acl_context.as_deref(), Some(acl.as_str()));
        assert_eq!(
            identity.acl.as_ref().map(|acl| acl.tenant.as_str()),
            Some("my-tenant")
        );
        assert_eq!(
            identity.principal_id.as_deref(),
            Some("3fa85f64-5717-4562-b3fc-2c963f66afa6")
        );
        assert_eq!(identity.request_id.as_deref(), Some("test-request-0001"));
        assert!(identity.bearer.is_some());
    }

    /// D47 — a malformed context is observed as `acl: None`, and the raw value is still carried
    /// so the engine sees exactly what the caller sent.
    #[rstest]
    fn test_a_malformed_acl_context_is_observed_without_a_rejection() {
        let request = mock_request(&[("x-mia-acl-context", "!!!not-base64!!!")]);

        let identity = MiaIdentity::observe(&request);

        assert_eq!(identity.acl, None);
        assert_eq!(identity.acl_context.as_deref(), Some("!!!not-base64!!!"));
    }

    #[rstest]
    fn test_an_empty_request_observes_nothing() {
        let identity = MiaIdentity::observe(&mock_request(&[]));

        assert_eq!(identity.acl_context, None);
        assert_eq!(identity.acl, None);
        assert_eq!(identity.principal_id, None);
        assert!(identity.bearer.is_none());
    }

    /// D45 — the bearer never prints, even through the layer's own type.
    #[rstest]
    fn test_the_observed_bearer_is_redacted() {
        let request = mock_request(&[("authorization", "Bearer test-token")]);

        let identity = MiaIdentity::observe(&request);

        assert!(!format!("{identity:?}").contains("test-token"));
    }

    /// §6.2 — one `From`, one direction, no validation in either.
    #[rstest]
    fn test_the_conversion_to_a_caller_identity_is_infallible_and_verbatim() {
        let acl = mock_acl_context();
        let observed = MiaIdentity::observe(&mock_request(&[
            ("x-mia-acl-context", &acl),
            ("x-mia-principal-id", "3fa85f64-5717-4562-b3fc-2c963f66afa6"),
        ]));

        let caller = CallerIdentity::from(&observed);

        assert_eq!(
            caller
                .forwarded()
                .get(ACL_CONTEXT_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(acl.as_str())
        );
        assert_eq!(caller.tenant_key().to_string(), "my-org/my-tenant");
    }

    /// Nothing in, nothing out — including through the conversion.
    #[rstest]
    fn test_an_empty_observation_converts_to_an_empty_identity() {
        let caller = CallerIdentity::from(&MiaIdentity::default());

        assert!(caller.forwarded().is_empty());
        assert_eq!(caller.tenant_key().to_string(), "unknown/unknown");
    }
}
