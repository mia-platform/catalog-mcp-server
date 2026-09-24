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
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Tenancy. **Tier 1 of the D26 allowlist**: forwarded byte-for-byte on every outbound request.
pub const ACL_CONTEXT_HEADER: HeaderName = HeaderName::from_static("x-mia-acl-context");

/// Ownership and attribution. **Tier 1 of the D26 allowlist**, and new to this server: the
/// previous one forwarded only `authorization` and `x-mia-acl-context`, which is why every agent
/// write is unattributed in revisions and audit today.
pub const PRINCIPAL_ID_HEADER: HeaderName = HeaderName::from_static("x-mia-principal-id");

/// The caller's bearer. **Tier 2 of the D26 allowlist**: passed through when present.
pub const AUTHORIZATION_HEADER: HeaderName = HeaderName::from_static("authorization");

/// Request correlation. **Tier 2 of the D26 allowlist**: passed through when present.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// What `organization` and `tenant` are reported as when no usable ACL context arrived (§7.2).
///
/// Absent or malformed identity is recorded and carried, never rejected (D47): the engine fails
/// the request authoritatively, and on a gateway-routed hop the policy has already replaced the
/// value anyway.
pub const UNKNOWN_TENANT: &str = "unknown";

/// A value that must never be logged and must not outlive its use (D45).
///
/// `secret_rs` is not vendored in this repository, so this is the local wrapper the decision
/// allows for. It wraps exactly one thing today: the forwarded bearer token.
#[derive(Clone, Default, PartialEq, Eq, ZeroizeOnDrop)]
pub struct Sensitive<T: Zeroize>(T);

impl<T: Zeroize> Sensitive<T> {
    /// Wraps a sensitive value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Borrows the wrapped value, at the one point where it has to be used.
    pub fn read(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> std::fmt::Debug for Sensitive<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T: Zeroize> std::fmt::Display for Sensitive<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// The decoded `x-mia-acl-context`, kept **only** for logging and tenancy (D47).
///
/// The header itself is forwarded as the received string and never re-encoded from this — that
/// is how a `tenantName` gets dropped.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct AclContext {
    /// The organization the caller is acting in.
    #[serde(rename = "organization")]
    pub organization: String,

    /// The tenant the caller is acting in.
    #[serde(rename = "tenant")]
    pub tenant: String,

    /// The tenant's display name, when the policy supplied one.
    #[serde(rename = "tenantName", default)]
    pub tenant_name: Option<String>,
}

impl AclContext {
    /// Decodes the header value: URL-safe base64 without padding, of a JSON object.
    ///
    /// Returns `None` rather than an error for anything unusable. **This is D47**: the server
    /// does not adjudicate identity, so a malformed context costs us a log field, not a request.
    pub fn decode(raw: &str) -> Option<Self> {
        let decoded = URL_SAFE_NO_PAD.decode(raw).ok()?;

        serde_json::from_slice(&decoded).ok()
    }

    /// The `{organization, tenant}` pair used as a log and metric field.
    pub fn tenant_key(&self) -> TenantKey {
        TenantKey {
            organization: self.organization.clone(),
            tenant: self.tenant.clone(),
        }
    }
}

/// The tenant log and metric field (§7.4).
///
/// **Nothing is keyed by it, because nothing is stored** (D31). It exists to label a span, a log
/// line and a rate-limit bucket, and for no other purpose.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TenantKey {
    /// The organization, or [`UNKNOWN_TENANT`] when no usable ACL context arrived.
    pub organization: String,

    /// The tenant, or [`UNKNOWN_TENANT`] when no usable ACL context arrived.
    pub tenant: String,
}

impl Default for TenantKey {
    fn default() -> Self {
        Self {
            organization: UNKNOWN_TENANT.to_string(),
            tenant: UNKNOWN_TENANT.to_string(),
        }
    }
}

impl std::fmt::Display for TenantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.organization, self.tenant)
    }
}

/// The caller's identity, as forwarded (§7.4).
///
/// **The identity pair is forwarded on every outbound engine request, unmodified (NFR-11, D26).**
/// That is a property of this type, not of each call site: [`Self::forwarded`] is built once, and
/// `EngineClient` exposes **no method taking a `HeaderMap`** — every request method applies it
/// itself, so a tool or an operation cannot construct a request that omits the pair.
///
/// Nothing here is validated and nothing is synthesised. No default tenant, no principal derived
/// from anything else: an absent principal id stays absent (D47), because inventing one would
/// attribute a write to somebody who did not make it, and a misattributed write is worse than an
/// unattributed one.
#[derive(Clone, Debug, Default)]
pub struct CallerIdentity {
    forwarded: HeaderMap,
    acl: Option<AclContext>,
    bearer: Option<Sensitive<String>>,
}

impl CallerIdentity {
    /// Builds the identity from what arrived, infallibly.
    ///
    /// `acl_context` and `principal_id` are the **raw** header strings: they are forwarded
    /// verbatim and are never re-encoded from a parsed value. `principal_id` is checked to be a
    /// UUID but forwarded as the original string, so canonicalisation cannot change it either.
    pub fn new(
        acl_context: Option<&str>,
        principal_id: Option<&str>,
        bearer: Option<&str>,
        request_id: Option<&str>,
    ) -> Self {
        let mut forwarded = HeaderMap::new();

        if let Some(raw) = acl_context
            && let Ok(value) = HeaderValue::from_str(raw)
        {
            forwarded.insert(ACL_CONTEXT_HEADER, value);
        }

        if let Some(raw) = principal_id
            && uuid::Uuid::parse_str(raw).is_ok()
            && let Ok(value) = HeaderValue::from_str(raw)
        {
            forwarded.insert(PRINCIPAL_ID_HEADER, value);
        }

        if let Some(raw) = bearer
            && let Ok(mut value) = HeaderValue::from_str(raw)
        {
            value.set_sensitive(true);
            forwarded.insert(AUTHORIZATION_HEADER, value);
        }

        if let Some(raw) = request_id
            && let Ok(value) = HeaderValue::from_str(raw)
        {
            forwarded.insert(REQUEST_ID_HEADER, value);
        }

        Self {
            acl: acl_context.and_then(AclContext::decode),
            bearer: bearer.map(|raw| Sensitive::new(raw.to_string())),
            forwarded,
        }
    }

    /// The headers to put on every outbound engine request: the D26 allowlist, nothing else.
    ///
    /// `acl-filter` is never here — it is policy-injected and a client must never send it — and
    /// neither is `x-jwt-payload`, which carries unverified claims by construction.
    pub fn forwarded(&self) -> &HeaderMap {
        &self.forwarded
    }

    /// The decoded ACL context, when one arrived and was usable. For logging and tenancy only.
    pub fn acl(&self) -> Option<&AclContext> {
        self.acl.as_ref()
    }

    /// The forwarded bearer, wrapped so it cannot be logged (D45).
    pub fn bearer(&self) -> Option<&Sensitive<String>> {
        self.bearer.as_ref()
    }

    /// The tenant log and metric field, or `unknown/unknown` when no usable context arrived.
    pub fn tenant_key(&self) -> TenantKey {
        self.acl
            .as_ref()
            .map(AclContext::tenant_key)
            .unwrap_or_default()
    }

    /// The principal id as received, for the log field that makes an audit trail followable.
    ///
    /// It is an actor id, not a secret, which is why — unlike the bearer and the raw ACL
    /// context — it **is** logged.
    pub fn principal_id(&self) -> Option<&str> {
        self.forwarded
            .get(PRINCIPAL_ID_HEADER)
            .and_then(|value| value.to_str().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A fictional ACL context, encoded exactly as the policy layer encodes one.
    fn mock_acl_header(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[rstest]
    fn test_acl_context_decodes_the_policy_encoding() {
        let raw = mock_acl_header(r#"{"organization":"my-org","tenant":"my-tenant"}"#);

        let acl = AclContext::decode(&raw).expect("a well-formed context decodes");

        assert_eq!(acl.organization, "my-org");
        assert_eq!(acl.tenant, "my-tenant");
        assert_eq!(acl.tenant_name, None);
    }

    #[rstest]
    fn test_acl_context_keeps_the_tenant_name() {
        let raw = mock_acl_header(
            r#"{"organization":"my-org","tenant":"my-tenant","tenantName":"My Tenant"}"#,
        );

        let acl = AclContext::decode(&raw).expect("a well-formed context decodes");

        assert_eq!(acl.tenant_name.as_deref(), Some("My Tenant"));
    }

    /// D47 — a malformed context costs a log field, not a request.
    #[rstest]
    #[case::not_base64("!!! not base64 !!!")]
    #[case::not_json("bm90IGpzb24")]
    #[case::missing_tenant("eyJvcmdhbml6YXRpb24iOiJteS1vcmcifQ")]
    fn test_unusable_acl_context_decodes_to_none(#[case] raw: &str) {
        assert_eq!(AclContext::decode(raw), None);
    }

    /// NFR-11 (b) — the bytes forwarded are byte-identical to the bytes received, including a
    /// `tenantName` that survives, because the header is carried and never re-encoded.
    #[rstest]
    fn test_acl_context_is_forwarded_verbatim() {
        let raw = mock_acl_header(
            r#"{"organization":"my-org","tenant":"my-tenant","tenantName":"My Tenant"}"#,
        );

        let identity = CallerIdentity::new(Some(&raw), None, None, None);

        assert_eq!(
            identity
                .forwarded()
                .get(ACL_CONTEXT_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(raw.as_str())
        );
    }

    /// NFR-11 (b) — the principal id is validated as a UUID and then forwarded as the original
    /// string, so canonicalisation cannot change its casing.
    #[rstest]
    fn test_principal_id_casing_is_not_canonicalised() {
        let principal = "3FA85F64-5717-4562-B3FC-2C963F66AFA6";

        let identity = CallerIdentity::new(None, Some(principal), None, None);

        assert_eq!(identity.principal_id(), Some(principal));
    }

    /// A principal id that is not a UUID is not forwarded — but it is not an error either
    /// (D47). The engine's own extractor is `Infallible` for the same reason.
    #[rstest]
    fn test_malformed_principal_id_is_dropped_without_an_error() {
        let identity = CallerIdentity::new(None, Some("not-a-uuid"), None, None);

        assert_eq!(identity.principal_id(), None);
        assert!(identity.forwarded().get(PRINCIPAL_ID_HEADER).is_none());
    }

    /// NFR-11 (c) — neither header is ever synthesised: with none inbound, none goes out.
    #[rstest]
    fn test_nothing_is_synthesised_when_nothing_arrives() {
        let identity = CallerIdentity::new(None, None, None, None);

        assert!(identity.forwarded().is_empty());
        assert_eq!(identity.acl(), None);
        assert_eq!(identity.principal_id(), None);
        assert!(identity.bearer().is_none());
    }

    /// §7.2 — an absent or malformed context records `tenant = unknown` and carries on.
    #[rstest]
    fn test_tenant_key_is_unknown_without_a_context() {
        let identity = CallerIdentity::new(None, None, None, None);

        assert_eq!(identity.tenant_key().to_string(), "unknown/unknown");
    }

    #[rstest]
    fn test_tenant_key_comes_from_the_decoded_context() {
        let raw = mock_acl_header(r#"{"organization":"my-org","tenant":"my-tenant"}"#);

        let identity = CallerIdentity::new(Some(&raw), None, None, None);

        assert_eq!(identity.tenant_key().to_string(), "my-org/my-tenant");
    }

    /// D45 — the bearer is never printable, by either formatter.
    #[rstest]
    fn test_bearer_is_redacted_in_both_formatters() {
        let identity = CallerIdentity::new(None, None, Some("Bearer test-token"), None);

        let bearer = identity.bearer().expect("a bearer was forwarded");

        assert_eq!(format!("{bearer:?}"), "[REDACTED]");
        assert_eq!(format!("{bearer}"), "[REDACTED]");
        assert!(!format!("{identity:?}").contains("test-token"));
    }

    /// D26 — nothing outside the allowlist is ever forwarded.
    #[rstest]
    fn test_only_the_allowlist_is_forwarded() {
        let raw = mock_acl_header(r#"{"organization":"my-org","tenant":"my-tenant"}"#);
        let identity = CallerIdentity::new(
            Some(&raw),
            Some("3fa85f64-5717-4562-b3fc-2c963f66afa6"),
            Some("Bearer test-token"),
            Some("test-request-0001"),
        );

        let names: Vec<String> = identity
            .forwarded()
            .keys()
            .map(|name| name.as_str().to_string())
            .collect();

        assert_eq!(names.len(), 4);
        for expected in [
            "x-mia-acl-context",
            "x-mia-principal-id",
            "authorization",
            "x-request-id",
        ] {
            assert!(names.iter().any(|name| name == expected), "{expected}");
        }
    }
}
