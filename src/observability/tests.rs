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
use crate::observability::{ALL_METRICS, Outcome, REMEDY_NONE};
use rstest::rstest;

/// §10 names exactly seven metrics, and they are exactly what the A/B needs. A new one is a
/// deliberate act, not an accident of instrumenting something.
#[rstest]
fn test_there_are_exactly_seven_metrics() {
    assert_eq!(ALL_METRICS.len(), 7);
}

#[rstest]
fn test_the_metric_names_are_the_documented_ones() {
    assert_eq!(
        ALL_METRICS,
        &[
            "mcp_tool_calls_total",
            "mcp_tool_duration_seconds",
            "mcp_response_bytes",
            "mcp_engine_requests_total",
            "mcp_engine_duration_seconds",
            "mcp_protocol_errors_total",
            "mcp_tools_list_bytes",
        ]
    );
}

#[rstest]
fn test_no_metric_is_named_twice() {
    let mut names = ALL_METRICS.to_vec();
    let before = names.len();

    names.sort_unstable();
    names.dedup();

    assert_eq!(names.len(), before);
}

/// `protocol_error` is **not** an outcome a handler can set: the transport rejects those before
/// any handler runs. The asymmetry is the point, and this is where it is pinned.
#[rstest]
fn test_protocol_error_is_not_a_handler_outcome() {
    for outcome in [Outcome::Ok, Outcome::ToolError, Outcome::Cancelled] {
        assert_ne!(outcome.as_str(), "protocol_error");
    }
}

#[rstest]
#[case(Outcome::Ok, "ok")]
#[case(Outcome::ToolError, "tool_error")]
#[case(Outcome::Cancelled, "cancelled")]
fn test_outcome_label_values(#[case] outcome: Outcome, #[case] expected: &str) {
    assert_eq!(outcome.as_str(), expected);
}

/// A successful call still carries a `remedy` label, because a Prometheus series cannot gain and
/// lose a label between scrapes without becoming two series.
#[rstest]
fn test_a_successful_call_has_a_remedy_label() {
    assert_eq!(REMEDY_NONE, "none");
}
