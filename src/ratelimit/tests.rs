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
use crate::ratelimit::{RateLimitDecision, RateLimiter};
use catalog_client::TenantKey;
use configuration::tools::RateLimitConfig;
use rstest::{fixture, rstest};
use std::time::{Duration, Instant};

/// A fictional tenant, per D39.
fn mock_tenant(name: &str) -> TenantKey {
    TenantKey {
        organization: "my-org".to_string(),
        tenant: name.to_string(),
    }
}

/// The shipped defaults: 120 calls a minute, burst 20.
#[fixture]
fn mock_limiter() -> RateLimiter {
    RateLimiter::new(&RateLimitConfig::default())
}

#[rstest]
fn test_a_fresh_tenant_starts_with_a_full_bucket(mock_limiter: RateLimiter) {
    let tenant = mock_tenant("tenant-one");

    for _ in 0..RateLimitConfig::default().burst {
        assert_eq!(mock_limiter.check(&tenant), RateLimitDecision::Allowed);
    }
}

/// The burst is the bucket, and the call after it is limited.
#[rstest]
fn test_exceeding_the_burst_is_limited(mock_limiter: RateLimiter) {
    let tenant = mock_tenant("tenant-one");

    for _ in 0..RateLimitConfig::default().burst {
        assert_eq!(mock_limiter.check(&tenant), RateLimitDecision::Allowed);
    }

    let RateLimitDecision::Limited { retry_after_ms } = mock_limiter.check(&tenant) else {
        panic!("the call after the burst must be limited");
    };

    // 120/minute is one token every 500 ms.
    assert!(
        retry_after_ms > 0 && retry_after_ms <= 500,
        "{retry_after_ms}"
    );
}

/// **Per tenant.** One tenant exhausting its bucket must not limit another — the bucket key is
/// the whole point.
#[rstest]
fn test_one_tenant_cannot_exhaust_anothers_bucket(mock_limiter: RateLimiter) {
    let first = mock_tenant("tenant-one");
    let second = mock_tenant("tenant-two");

    for _ in 0..RateLimitConfig::default().burst + 5 {
        let _ = mock_limiter.check(&first);
    }

    assert!(matches!(
        mock_limiter.check(&first),
        RateLimitDecision::Limited { .. }
    ));
    assert_eq!(mock_limiter.check(&second), RateLimitDecision::Allowed);
}

/// An unknown tenant is still a tenant: `unknown/unknown` gets its own bucket rather than
/// bypassing the limiter or sharing everybody else's.
#[rstest]
fn test_an_unknown_tenant_gets_its_own_bucket(mock_limiter: RateLimiter) {
    let unknown = TenantKey::default();

    assert_eq!(mock_limiter.check(&unknown), RateLimitDecision::Allowed);
}

/// Tokens come back at the configured rate.
#[rstest]
fn test_the_bucket_refills_over_time() {
    let limiter = RateLimiter::new(&RateLimitConfig {
        enabled: true,
        per_tenant_calls_per_minute: 60,
        burst: 2,
    });
    let tenant = mock_tenant("tenant-one");
    let start = Instant::now();

    assert_eq!(limiter.check_at(&tenant, start), RateLimitDecision::Allowed);
    assert_eq!(limiter.check_at(&tenant, start), RateLimitDecision::Allowed);
    assert!(matches!(
        limiter.check_at(&tenant, start),
        RateLimitDecision::Limited { .. }
    ));

    // 60/minute is one token a second.
    assert_eq!(
        limiter.check_at(&tenant, start + Duration::from_secs(1)),
        RateLimitDecision::Allowed
    );
}

/// The bucket never refills past its capacity, so an idle tenant cannot bank a day's traffic.
#[rstest]
fn test_the_bucket_does_not_refill_past_its_capacity() {
    let limiter = RateLimiter::new(&RateLimitConfig {
        enabled: true,
        per_tenant_calls_per_minute: 60,
        burst: 2,
    });
    let tenant = mock_tenant("tenant-one");
    let start = Instant::now();

    let _ = limiter.check_at(&tenant, start);
    let later = start + Duration::from_secs(3_600);

    assert_eq!(limiter.check_at(&tenant, later), RateLimitDecision::Allowed);
    assert_eq!(limiter.check_at(&tenant, later), RateLimitDecision::Allowed);
    assert!(matches!(
        limiter.check_at(&tenant, later),
        RateLimitDecision::Limited { .. }
    ));
}

#[rstest]
fn test_a_disabled_limiter_allows_everything() {
    let limiter = RateLimiter::new(&RateLimitConfig {
        enabled: false,
        per_tenant_calls_per_minute: 1,
        burst: 1,
    });
    let tenant = mock_tenant("tenant-one");

    for _ in 0..100 {
        assert_eq!(limiter.check(&tenant), RateLimitDecision::Allowed);
    }
}

/// `retryAfterMs` is what makes `RetryLater` actionable, so it must be a usable number rather
/// than zero.
#[rstest]
fn test_the_retry_hint_is_usable() {
    let limiter = RateLimiter::new(&RateLimitConfig {
        enabled: true,
        per_tenant_calls_per_minute: 60,
        burst: 1,
    });
    let tenant = mock_tenant("tenant-one");
    let start = Instant::now();

    let _ = limiter.check_at(&tenant, start);
    let RateLimitDecision::Limited { retry_after_ms } = limiter.check_at(&tenant, start) else {
        panic!("the second call must be limited");
    };

    assert_eq!(retry_after_ms, 1_000);
}

/// **Every caller without a usable ACL context shares one bucket.**
///
/// `TenantKey` is what the limiter keys on, and an absent or malformed context decodes to
/// `unknown/unknown` (§7.2, D47) — so two different unidentified callers are one tenant as far
/// as the limiter is concerned, and either can exhaust the other's budget.
///
/// That is a consequence of keying on tenancy rather than on the caller, which is what §6.4
/// asks for. It is pinned here rather than left to be discovered: on the gateway path every
/// request carries a context, so the shared bucket is only reachable from the in-cluster path —
/// which §7.1 already describes as trusted-network and §13.7 already wants a NetworkPolicy in
/// front of.
#[rstest]
fn test_every_unidentified_caller_shares_one_bucket(mock_limiter: RateLimiter) {
    let unknown = TenantKey::default();

    for _ in 0..RateLimitConfig::default().burst {
        assert_eq!(mock_limiter.check(&unknown), RateLimitDecision::Allowed);
    }

    // A second caller, equally unidentified, meets the first one's exhausted bucket.
    assert!(matches!(
        mock_limiter.check(&TenantKey::default()),
        RateLimitDecision::Limited { .. }
    ));
}
