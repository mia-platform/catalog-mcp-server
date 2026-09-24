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
use crate::warning::{EngineWarning, parse};
use http::{HeaderMap, HeaderValue, header::WARNING};
use rstest::rstest;

/// Builds a header map carrying the given `Warning` values, in order.
fn mock_warnings(values: &[&str]) -> HeaderMap {
    let mut headers = HeaderMap::new();

    for value in values {
        headers.append(
            WARNING,
            HeaderValue::from_str(value).expect("a well-formed test header value"),
        );
    }

    headers
}

#[rstest]
fn test_no_warning_header_yields_an_empty_vector() {
    assert_eq!(parse(&HeaderMap::new()), vec![]);
}

#[rstest]
fn test_one_warning() {
    let headers = mock_warnings(&[r#"299 - "The 'customFields' field was ignored.""#]);

    assert_eq!(
        parse(&headers),
        vec![EngineWarning {
            code: 299,
            text: "The 'customFields' field was ignored.".to_string(),
        }]
    );
}

/// The header is repeatable and **all** of them are collected, in order.
#[rstest]
fn test_many_warnings_are_all_collected_in_order() {
    let headers = mock_warnings(&[r#"299 - "first""#, r#"299 - "second""#, r#"299 - "third""#]);

    let texts: Vec<String> = parse(&headers).into_iter().map(|w| w.text).collect();

    assert_eq!(texts, vec!["first", "second", "third"]);
}

/// The engine does not escape inner quotes, so the parser must not either: everything between
/// the outer quotes is the message.
#[rstest]
fn test_embedded_quotes_survive() {
    let headers = mock_warnings(&[r#"299 - "'spec.group' field is read-only.""#]);

    assert_eq!(parse(&headers)[0].text, "'spec.group' field is read-only.");
}

#[rstest]
#[case::no_code(r#" - "no code""#)]
#[case::no_quotes("299 - unquoted")]
#[case::empty("")]
#[case::wrong_separator(r#"299: "wrong separator""#)]
#[case::trailing_garbage(r#"299 - "text" extra"#)]
fn test_a_malformed_value_is_dropped_rather_than_guessed_at(#[case] raw: &str) {
    assert_eq!(parse(&mock_warnings(&[raw])), vec![]);
}

/// A malformed warning must not take a good one with it.
#[rstest]
fn test_a_malformed_value_does_not_discard_the_others() {
    let headers = mock_warnings(&["nonsense", r#"299 - "kept""#]);

    assert_eq!(parse(&headers).len(), 1);
    assert_eq!(parse(&headers)[0].text, "kept");
}

/// The code is parsed, not assumed, even though this engine only ever sends `299`.
#[rstest]
fn test_the_code_is_parsed() {
    let headers = mock_warnings(&[r#"214 - "Transformation Applied""#]);

    assert_eq!(parse(&headers)[0].code, 214);
}

/// D28 — T12's `ignored` list is derived from the warning by a named regex, not by each tool
/// matching on prose.
#[rstest]
fn test_read_only_field_is_extracted() {
    let warning = EngineWarning {
        code: 299,
        text: "'spec.group' field is read-only and was ignored during the update.".to_string(),
    };

    assert_eq!(warning.read_only_field(), Some("spec.group"));
}

#[rstest]
fn test_an_unrelated_warning_yields_no_read_only_field() {
    let warning = EngineWarning {
        code: 299,
        text: "The 'customFields' field cannot be set through this endpoint.".to_string(),
    };

    assert_eq!(warning.read_only_field(), None);
}

#[rstest]
fn test_display_round_trips_the_wire_form() {
    let warning = EngineWarning {
        code: 299,
        text: "something happened".to_string(),
    };

    assert_eq!(warning.to_string(), r#"299 - "something happened""#);
}
