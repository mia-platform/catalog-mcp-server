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
use catalog_client::TenantKey;
use configuration::tools::RateLimitConfig;
use std::{collections::HashMap, sync::Mutex, time::Instant};

/// Seconds in a minute, for turning a per-minute rate into a refill rate.
const SECONDS_PER_MINUTE: f64 = 60.0;

/// One tenant's bucket.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// A token bucket keyed by tenant (§6.4).
///
/// Checked in `call_tool` **after** identity and **before** the tool runs. `list_tools` and
/// `get_tool` are not limited: they are prebuilt and free.
///
/// **The buckets are per replica**, so the effective cluster limit is `replicas × rate`. That is
/// the kind of arithmetic that surprises an operator during an incident, so it is written here
/// as well as in the plan. With one replica today they are the same number.
#[derive(Debug)]
pub struct RateLimiter {
    enabled: bool,
    capacity: f64,
    refill_per_second: f64,
    buckets: Mutex<HashMap<TenantKey, Bucket>>,
}

/// What a rate-limit check decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitDecision {
    /// There was a token, and it has been taken.
    Allowed,

    /// There was not. The caller is told how long to wait, because the whole point of
    /// `RetryLater` is that the model can act on it.
    Limited {
        /// Milliseconds until the next token, for `details.retryAfterMs`.
        retry_after_ms: u64,
    },
}

impl RateLimiter {
    /// Builds the limiter from configuration.
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            enabled: config.enabled,
            capacity: f64::from(config.burst.max(1)),
            refill_per_second: f64::from(config.per_tenant_calls_per_minute) / SECONDS_PER_MINUTE,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `tenant`, or says how long to wait.
    pub fn check(&self, tenant: &TenantKey) -> RateLimitDecision {
        self.check_at(tenant, Instant::now())
    }

    /// [`Self::check`] against a caller-supplied clock, which is what makes it testable without
    /// sleeping.
    fn check_at(&self, tenant: &TenantKey, now: Instant) -> RateLimitDecision {
        if !self.enabled {
            return RateLimitDecision::Allowed;
        }

        // PANIC: the lock is held only for the arithmetic below, which cannot panic, so it
        // cannot be poisoned. Recovering from poisoning anyway would mean inventing a bucket
        // state, and refusing traffic because of a lock is worse than serving it.
        let mut buckets = match self.buckets.lock() {
            Ok(buckets) => buckets,
            Err(poisoned) => poisoned.into_inner(),
        };

        let bucket = buckets.entry(tenant.clone()).or_insert(Bucket {
            tokens: self.capacity,
            last_refill: now,
        });

        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_second).min(self.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;

            return RateLimitDecision::Allowed;
        }

        let missing = 1.0 - bucket.tokens;
        let seconds = if self.refill_per_second > 0.0 {
            missing / self.refill_per_second
        } else {
            SECONDS_PER_MINUTE
        };

        RateLimitDecision::Limited {
            retry_after_ms: (seconds * 1_000.0).ceil() as u64,
        }
    }
}

#[cfg(test)]
mod tests;
