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
use serde::Deserialize;

/// Default wall-clock budget for one `tools/call`, in seconds (§6.4): inside a typical
/// client tool-call timeout, and what bounds T4's poll.
pub static DEFAULT_CALL_DEADLINE_SECONDS: u64 = 25;

/// Default `ttlMs` advertised with `tools/list` (D13). The set changes only on deploy.
pub static DEFAULT_TOOLS_LIST_TTL_MS: u64 = 3_600_000;

/// Whether the per-tenant limiter runs when the configuration does not say.
///
/// **Off in v1, by the owner's decision of 24 Sep 2026**, and a deliberate departure from the
/// specification's *"Servers MUST … Rate limit tool invocations"* (§6.4). The previous server had
/// no limiting anywhere and neither does the gateway, so this is parity with what production has
/// always run. The limiter and its `rate_limited` error stay, so an environment can turn it on
/// without a release.
pub static DEFAULT_RATE_LIMIT_ENABLED: bool = false;

/// Default per-tenant call allowance per minute, applied only when the limiter is enabled (§6.4).
pub static DEFAULT_PER_TENANT_CALLS_PER_MINUTE: u32 = 120;

/// Default burst capacity of the per-tenant token bucket.
pub static DEFAULT_RATE_LIMIT_BURST: u32 = 20;

/// Default `limit` for catalog searches — the engine's own default.
pub static DEFAULT_SEARCH_LIMIT: u32 = 50;

/// Default `limit` for relationship listings — the engine's own default.
pub static DEFAULT_RELATIONSHIP_LIMIT: u32 = 50;

/// Default `limit` for item history listings.
pub static DEFAULT_HISTORY_LIMIT: u32 = 10;

/// Default `limit` for audit-log listings.
pub static DEFAULT_AUDIT_LIMIT: u32 = 25;

/// Default time a compliance evaluation is waited on, in seconds.
pub static DEFAULT_COMPLIANCE_WAIT_SECONDS: u64 = 20;

/// Default ceiling on the bytes a caller may send us in one write (NFR-10).
pub static DEFAULT_MAX_WRITE_BYTES: usize = 262_144;

/// Returns [`DEFAULT_CALL_DEADLINE_SECONDS`].
pub fn default_call_deadline_seconds() -> u64 {
    DEFAULT_CALL_DEADLINE_SECONDS
}

/// Returns [`DEFAULT_TOOLS_LIST_TTL_MS`].
pub fn default_tools_list_ttl_ms() -> u64 {
    DEFAULT_TOOLS_LIST_TTL_MS
}

/// Returns [`DEFAULT_RATE_LIMIT_ENABLED`].
pub fn default_rate_limit_enabled() -> bool {
    DEFAULT_RATE_LIMIT_ENABLED
}

/// Returns [`DEFAULT_PER_TENANT_CALLS_PER_MINUTE`].
pub fn default_per_tenant_calls_per_minute() -> u32 {
    DEFAULT_PER_TENANT_CALLS_PER_MINUTE
}

/// Returns [`DEFAULT_RATE_LIMIT_BURST`].
pub fn default_rate_limit_burst() -> u32 {
    DEFAULT_RATE_LIMIT_BURST
}

/// Returns [`DEFAULT_SEARCH_LIMIT`].
pub fn default_search_limit() -> u32 {
    DEFAULT_SEARCH_LIMIT
}

/// Returns [`DEFAULT_RELATIONSHIP_LIMIT`].
pub fn default_relationship_limit() -> u32 {
    DEFAULT_RELATIONSHIP_LIMIT
}

/// Returns [`DEFAULT_HISTORY_LIMIT`].
pub fn default_history_limit() -> u32 {
    DEFAULT_HISTORY_LIMIT
}

/// Returns [`DEFAULT_AUDIT_LIMIT`].
pub fn default_audit_limit() -> u32 {
    DEFAULT_AUDIT_LIMIT
}

/// Returns [`DEFAULT_COMPLIANCE_WAIT_SECONDS`].
pub fn default_compliance_wait_seconds() -> u64 {
    DEFAULT_COMPLIANCE_WAIT_SECONDS
}

/// Returns [`DEFAULT_MAX_WRITE_BYTES`].
pub fn default_max_write_bytes() -> usize {
    DEFAULT_MAX_WRITE_BYTES
}

/// The per-tenant token bucket of §6.4. Buckets are per replica, so the effective cluster
/// limit is `replicas × rate`.
///
/// **Off by default in v1**: it limits nothing unless `enabled` is set to `true`.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct RateLimitConfig {
    /// Whether the limiter runs at all.
    #[serde(default = "default_rate_limit_enabled", rename = "enabled")]
    pub enabled: bool,

    /// Refill rate, in calls per minute, per tenant.
    #[serde(
        default = "default_per_tenant_calls_per_minute",
        rename = "perTenantCallsPerMinute"
    )]
    pub per_tenant_calls_per_minute: u32,

    /// Bucket capacity.
    #[serde(default = "default_rate_limit_burst", rename = "burst")]
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: default_rate_limit_enabled(),
            per_tenant_calls_per_minute: default_per_tenant_calls_per_minute(),
            burst: default_rate_limit_burst(),
        }
    }
}

/// Every tunable the tool analyses marked as a guess (§11). They live here, not in the code,
/// because the measurement exercise will change them and a config change is not a release.
///
/// **No response ceilings are among them** — there are none (D34).
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ToolDefaults {
    /// Default `limit` for catalog searches.
    #[serde(default = "default_search_limit", rename = "searchLimit")]
    pub search_limit: u32,

    /// Default `limit` for relationship listings.
    #[serde(default = "default_relationship_limit", rename = "relationshipLimit")]
    pub relationship_limit: u32,

    /// Default `limit` for item history listings.
    #[serde(default = "default_history_limit", rename = "historyLimit")]
    pub history_limit: u32,

    /// Default `limit` for audit-log listings.
    #[serde(default = "default_audit_limit", rename = "auditLimit")]
    pub audit_limit: u32,

    /// How long a compliance evaluation is waited on, in seconds.
    #[serde(
        default = "default_compliance_wait_seconds",
        rename = "complianceWaitSeconds"
    )]
    pub compliance_wait_seconds: u64,

    /// Ceiling on the bytes a caller may send us in one write.
    #[serde(default = "default_max_write_bytes", rename = "maxWriteBytes")]
    pub max_write_bytes: usize,
}

impl Default for ToolDefaults {
    fn default() -> Self {
        Self {
            search_limit: default_search_limit(),
            relationship_limit: default_relationship_limit(),
            history_limit: default_history_limit(),
            audit_limit: default_audit_limit(),
            compliance_wait_seconds: default_compliance_wait_seconds(),
            max_write_bytes: default_max_write_bytes(),
        }
    }
}

/// What bounds a tool call and what the tool set advertises (§11).
///
/// `tools_list_ttl_ms` sits here rather than under `transport` because it is a property of the
/// tool set (D13), not of the wire.
#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(::schemars::JsonSchema))]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ToolsConfig {
    /// Wall-clock budget for one `tools/call`, in seconds.
    #[serde(
        default = "default_call_deadline_seconds",
        rename = "callDeadlineSeconds"
    )]
    pub call_deadline_seconds: u64,

    /// `ttlMs` advertised with `tools/list`.
    #[serde(default = "default_tools_list_ttl_ms", rename = "toolsListTtlMs")]
    pub tools_list_ttl_ms: u64,

    /// The per-tenant token bucket.
    #[serde(default, rename = "rateLimit")]
    pub rate_limit: RateLimitConfig,

    /// Per-tool defaults and input ceilings.
    #[serde(default, rename = "defaults")]
    pub defaults: ToolDefaults,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            call_deadline_seconds: default_call_deadline_seconds(),
            tools_list_ttl_ms: default_tools_list_ttl_ms(),
            rate_limit: RateLimitConfig::default(),
            defaults: ToolDefaults::default(),
        }
    }
}
