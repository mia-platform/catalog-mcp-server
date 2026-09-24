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
use crate::error::{
    ALL_CODES, BadRequestOrigin, Dispatched, Remedy, ToolError, codes, deadline_exceeded,
    map_status, transport_failure,
};
use rstest::rstest;
use serde_json::json;

/// §8.4's two tables, transcribed. The set of codes reachable in the binary must equal this
/// union, so an error added without a documented row fails CI.
const DOCUMENTED_CODES: &[&str] = &[
    // First table — raised from an engine outcome.
    "invalid_input",
    "server_defect",
    "unauthenticated",
    "forbidden",
    "not_found",
    "conflict",
    "unsupported_for_type",
    "not_implemented",
    "upstream_unavailable",
    "catalog_unavailable",
    "unknown_outcome",
    "deadline_exceeded",
    // Second table — raised by the runtime and the client.
    "invalid_arguments",
    "invalid_cursor",
    "unaddressable_item",
    "unaddressable_type",
    "query_too_large",
    "cancelled",
    "rate_limited",
];

/// D19 is only true if every code is listed in one place. This is that assertion.
#[rstest]
fn test_code_set_matches_the_documented_tables() {
    let mut reachable: Vec<&str> = ALL_CODES.to_vec();
    let mut documented: Vec<&str> = DOCUMENTED_CODES.to_vec();

    reachable.sort_unstable();
    documented.sort_unstable();

    assert_eq!(reachable, documented);
}

#[rstest]
fn test_no_code_is_listed_twice() {
    let mut seen: Vec<&str> = ALL_CODES.to_vec();
    let before = seen.len();

    seen.sort_unstable();
    seen.dedup();

    assert_eq!(seen.len(), before);
}

// ---------------------------------------------------------------------------------------------
// §8.4's first table, one case per row, asserting `code` **and** `remedy`.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case::schema_validation(
    400,
    BadRequestOrigin::CallerInput,
    codes::INVALID_INPUT,
    Remedy::RetryAfterChange
)]
#[case::parameter_we_built(
    400,
    BadRequestOrigin::ServerBuilt,
    codes::SERVER_DEFECT,
    Remedy::Escalate
)]
#[case::unauthenticated(
    401,
    BadRequestOrigin::CallerInput,
    codes::UNAUTHENTICATED,
    Remedy::Escalate
)]
#[case::forbidden(403, BadRequestOrigin::CallerInput, codes::FORBIDDEN, Remedy::Escalate)]
#[case::not_found(
    404,
    BadRequestOrigin::CallerInput,
    codes::NOT_FOUND,
    Remedy::RetryAfterChange
)]
#[case::not_acceptable(
    406,
    BadRequestOrigin::CallerInput,
    codes::SERVER_DEFECT,
    Remedy::Escalate
)]
#[case::conflict(
    409,
    BadRequestOrigin::CallerInput,
    codes::CONFLICT,
    Remedy::RetryLater
)]
#[case::unsupported_media(
    415,
    BadRequestOrigin::CallerInput,
    codes::SERVER_DEFECT,
    Remedy::Escalate
)]
#[case::unprocessable(
    422,
    BadRequestOrigin::CallerInput,
    codes::UNSUPPORTED_FOR_TYPE,
    Remedy::Escalate
)]
#[case::not_implemented(
    501,
    BadRequestOrigin::CallerInput,
    codes::NOT_IMPLEMENTED,
    Remedy::Escalate
)]
#[case::authz_upstream(
    502,
    BadRequestOrigin::CallerInput,
    codes::UPSTREAM_UNAVAILABLE,
    Remedy::Retry
)]
#[case::read_500(
    500,
    BadRequestOrigin::CallerInput,
    codes::CATALOG_UNAVAILABLE,
    Remedy::Retry
)]
#[case::read_503(
    503,
    BadRequestOrigin::CallerInput,
    codes::CATALOG_UNAVAILABLE,
    Remedy::Retry
)]
fn test_engine_status_maps_to_its_documented_row(
    #[case] status: u16,
    #[case] origin: BadRequestOrigin,
    #[case] expected_code: &str,
    #[case] expected_remedy: Remedy,
) {
    let error = map_status(status, origin, Dispatched::No, None, None);

    assert_eq!(error.code, expected_code);
    assert_eq!(error.remedy, expected_remedy);
}

/// T9's case, and the reason `Remedy::Unknown` exists: a `5XX` after a delete has taken effect
/// is indistinguishable from one before it.
#[rstest]
#[case::server_error(500)]
#[case::bad_gateway(502)]
#[case::unavailable(503)]
#[case::gateway_timeout(504)]
fn test_a_dispatched_write_that_fails_is_never_a_clean_failure(#[case] status: u16) {
    let error = map_status(
        status,
        BadRequestOrigin::CallerInput,
        Dispatched::Yes,
        None,
        None,
    );

    assert_eq!(error.code, codes::UNKNOWN_OUTCOME);
    assert_eq!(error.remedy, Remedy::Unknown);
    assert!(error.message.contains("may have taken effect"));
}

/// A `4xx` on a dispatched write is still a clean failure: the engine rejected it before doing
/// anything, and reporting `unknown` there would make the model verify for no reason.
#[rstest]
fn test_a_dispatched_write_rejected_with_4xx_is_a_clean_failure() {
    let error = map_status(
        409,
        BadRequestOrigin::CallerInput,
        Dispatched::Yes,
        None,
        None,
    );

    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(error.remedy, Remedy::RetryLater);
}

#[rstest]
fn test_transport_failure_on_a_read_is_unavailable_not_empty() {
    let error = transport_failure(Dispatched::No, None);

    assert_eq!(error.code, codes::CATALOG_UNAVAILABLE);
    assert_eq!(error.remedy, Remedy::Retry);
    assert!(error.message.contains("not the same as an empty result"));
}

#[rstest]
fn test_transport_failure_after_dispatch_is_unknown() {
    let error = transport_failure(Dispatched::Yes, None);

    assert_eq!(error.code, codes::UNKNOWN_OUTCOME);
    assert_eq!(error.remedy, Remedy::Unknown);
}

#[rstest]
fn test_deadline_on_a_read_is_retryable() {
    let error = deadline_exceeded(Dispatched::No, None);

    assert_eq!(error.code, codes::DEADLINE_EXCEEDED);
    assert_eq!(error.remedy, Remedy::Retry);
}

/// D20's deadline half: the same row as the 5xx case.
#[rstest]
fn test_deadline_after_a_dispatched_write_is_unknown() {
    let error = deadline_exceeded(Dispatched::Yes, None);

    assert_eq!(error.code, codes::UNKNOWN_OUTCOME);
    assert_eq!(error.remedy, Remedy::Unknown);
}

/// The engine's `500`s say only *"Something went wrong"*, so the message is ours and its
/// `x-request-id` is the one actionable thing a human gets.
#[rstest]
fn test_the_engine_request_id_is_carried_into_details() {
    let error = map_status(
        500,
        BadRequestOrigin::CallerInput,
        Dispatched::No,
        Some("Something went wrong"),
        Some("engine-request-0001"),
    );

    assert_eq!(
        error.details.expect("details carry the request id")["requestId"],
        json!("engine-request-0001")
    );
}

/// A `401` is phrased as an identity problem, never as a catalog one (T11).
#[rstest]
fn test_unauthenticated_is_not_phrased_as_a_catalog_problem() {
    let error = map_status(
        401,
        BadRequestOrigin::CallerInput,
        Dispatched::No,
        None,
        None,
    );

    assert!(error.message.contains("authentication"));
    assert!(!error.message.to_lowercase().contains("catalog problem"));
}

/// A `502` says **authz**, not *the catalog* (T11).
#[rstest]
fn test_upstream_unavailable_names_the_authorization_service() {
    let error = map_status(
        502,
        BadRequestOrigin::CallerInput,
        Dispatched::No,
        None,
        None,
    );

    assert!(error.message.contains("authorization service"));
}

// ---------------------------------------------------------------------------------------------
// The rendered shape.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_payload_shape_is_the_frozen_one() {
    let error = ToolError::new(
        codes::NOT_FOUND,
        Remedy::RetryAfterChange,
        "No such item in this tenant.",
    )
    .with_details(json!({ "candidates": ["example-item"] }))
    .with_next_step("call search_catalog to find the right name");

    assert_eq!(
        error.to_payload(),
        json!({
            "error": {
                "code": "not_found",
                "remedy": "retry_after_change",
                "message": "No such item in this tenant.",
                "details": { "candidates": ["example-item"] },
                "nextStep": "call search_catalog to find the right name",
            }
        })
    );
}

/// `details` and `nextStep` are omitted rather than null when there is nothing to say: they cost
/// bytes on every error otherwise.
#[rstest]
fn test_optional_fields_are_omitted_when_absent() {
    let error = ToolError::new(codes::CANCELLED, Remedy::Retry, "The caller went away.");

    assert_eq!(
        error.to_payload(),
        json!({
            "error": {
                "code": "cancelled",
                "remedy": "retry",
                "message": "The caller went away.",
            }
        })
    );
}

#[rstest]
#[case(Remedy::Retry, "retry")]
#[case(Remedy::RetryAfterChange, "retry_after_change")]
#[case(Remedy::RetryLater, "retry_later")]
#[case(Remedy::Escalate, "escalate")]
#[case(Remedy::Unknown, "unknown")]
fn test_remedy_wire_values(#[case] remedy: Remedy, #[case] expected: &str) {
    assert_eq!(remedy.as_str(), expected);
    assert_eq!(
        serde_json::to_value(remedy).expect("serialisable"),
        json!(expected)
    );
}
