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
use std::time::Duration;
use tokio::time::{Instant, error::Elapsed, timeout};

/// The wall-clock budget for one whole tool call (§5.5, §6.4).
///
/// Set once per call from `tools.callDeadlineSeconds` and consulted by `EngineClient` before
/// every request, so **`engine.timeoutMs` bounds one hop and this bounds the whole call**:
/// whichever is smaller wins, and a deadline with no time left fails without dialling.
///
/// It is a clone-cheap value rather than a guard: a tool may hand it to several concurrent
/// engine calls, and all of them are bounded by the same instant.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    at: Instant,
}

impl Deadline {
    /// Starts a deadline `budget` from now.
    pub fn starting_now(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
        }
    }

    /// How long is left. Zero once the deadline has passed — never negative.
    pub fn remaining(&self) -> Duration {
        self.at.saturating_duration_since(Instant::now())
    }

    /// Whether there is no time left.
    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }

    /// Bounds a future by whatever is left of the budget.
    pub async fn bounded<F: Future>(&self, future: F) -> Result<F::Output, Elapsed> {
        timeout(self.remaining(), future).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[tokio::test]
    async fn test_a_fresh_deadline_has_time_left() {
        let deadline = Deadline::starting_now(Duration::from_secs(25));

        assert!(!deadline.expired());
        assert!(deadline.remaining() > Duration::from_secs(20));
    }

    #[rstest]
    #[tokio::test]
    async fn test_an_elapsed_deadline_reports_zero_rather_than_underflowing() {
        tokio::time::pause();
        let deadline = Deadline::starting_now(Duration::from_secs(1));
        tokio::time::advance(Duration::from_secs(5)).await;

        assert!(deadline.expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
    }

    #[rstest]
    #[tokio::test]
    async fn test_bounded_lets_a_prompt_future_through() {
        let deadline = Deadline::starting_now(Duration::from_secs(25));

        let result = deadline.bounded(async { 42 }).await;

        assert_eq!(result.expect("a prompt future is not cut off"), 42);
    }

    #[rstest]
    #[tokio::test]
    async fn test_bounded_cuts_off_a_slow_future() {
        tokio::time::pause();
        let deadline = Deadline::starting_now(Duration::from_millis(10));

        let result = deadline
            .bounded(tokio::time::sleep(Duration::from_secs(60)))
            .await;

        assert!(result.is_err());
    }

    /// A deadline with nothing left admits no work that has to wait.
    ///
    /// Note what this does **not** claim: `tokio::time::timeout` polls its future once before
    /// checking the clock, so an already-ready future still completes on an expired deadline.
    /// That is why `EngineClient` consults [`Deadline::expired`] before dialling rather than
    /// relying on `bounded` alone — a request is never ready on its first poll, but the
    /// distinction is worth pinning rather than discovering.
    #[rstest]
    #[tokio::test]
    async fn test_an_expired_deadline_admits_no_waiting_work() {
        tokio::time::pause();
        let deadline = Deadline::starting_now(Duration::from_secs(1));
        tokio::time::advance(Duration::from_secs(5)).await;

        assert!(deadline.expired());
        assert!(
            deadline
                .bounded(tokio::time::sleep(Duration::from_millis(1)))
                .await
                .is_err()
        );
    }
}
