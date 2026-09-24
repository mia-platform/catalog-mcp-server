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
use crate::{
    error::{Remedy, codes},
    query::{
        FieldPath, MAX_BRANCH_CHILDREN, MAX_DEPTH, MAX_LEAF_PREDICATES, MAX_RAWQ_PARAM_BYTES,
        MAX_RAWQ_PARAMS, MAX_REGEX_BYTES, MAX_VALUE_BYTES, Predicate, QueryValue, RegexLiteral,
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rstest::rstest;
use serde_json::{Value, json};

/// A validated field path, for a test that is not about validation.
fn field(path: &str) -> FieldPath {
    FieldPath::new(path).expect("a field the engine accepts")
}

/// `metadata.name eq <value>`.
fn eq(path: &str, value: &str) -> Predicate {
    Predicate::Eq {
        field: field(path),
        value: QueryValue::string(value).expect("a value within the cap"),
    }
}

/// Decodes one encoded `rawq` parameter back to JSON, as the engine does.
fn decode(parameter: &str) -> Value {
    let bytes = URL_SAFE_NO_PAD
        .decode(parameter)
        .expect("the engine decodes URL-safe base64 without padding");

    serde_json::from_slice(&bytes).expect("the engine parses the decoded JSON")
}

// ---------------------------------------------------------------------------------------------
// One test per operator.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_eq_has_the_engines_shape() {
    assert_eq!(
        eq("metadata.name", "example-item").to_json(),
        json!({ "metadata.name": { "eq": "example-item" } })
    );
}

/// The wire name is **snake_case**, and the value is a regex **literal** rather than a bare
/// string — the two corrections this plan makes to T2's analysis.
#[rstest]
fn test_matches_has_the_engines_shape() {
    let predicate = Predicate::Matches {
        field: field("metadata.title"),
        pattern: RegexLiteral::containing("gateway").expect("a short literal"),
    };

    assert_eq!(
        predicate.to_json(),
        json!({ "metadata.title": { "matches": "/gateway/i" } })
    );
}

#[rstest]
fn test_and_has_the_engines_shape() {
    let predicate = Predicate::And(vec![
        eq("kind", "Service"),
        eq("metadata.name", "example-item"),
    ]);

    assert_eq!(
        predicate.to_json(),
        json!({
            "and": [
                { "kind": { "eq": "Service" } },
                { "metadata.name": { "eq": "example-item" } },
            ]
        })
    );
}

#[rstest]
fn test_or_has_the_engines_shape() {
    let predicate = Predicate::Or(vec![eq("kind", "Service"), eq("kind", "Bucket")]);

    assert_eq!(
        predicate.to_json(),
        json!({ "or": [{ "kind": { "eq": "Service" } }, { "kind": { "eq": "Bucket" } }] })
    );
}

/// Every value kind the AST carries renders as the engine's untagged `QueryValue`.
#[rstest]
#[case(QueryValue::Integer(2), json!(2))]
#[case(QueryValue::Number(1.5), json!(1.5))]
#[case(QueryValue::Boolean(true), json!(true))]
#[case(QueryValue::Null, Value::Null)]
fn test_value_kinds_render_untagged(#[case] value: QueryValue, #[case] expected: Value) {
    let predicate = Predicate::Eq {
        field: field("spec.replicas"),
        value,
    };

    assert_eq!(predicate.to_json()["spec.replicas"]["eq"], expected);
}

// ---------------------------------------------------------------------------------------------
// The free-text search shape T2 composes from these parts.
// ---------------------------------------------------------------------------------------------

/// A free-text `query` becomes an `or` of three `matches` over `metadata.{name,title,tags}`.
///
/// The composition is the tool's, not the translator's — but the shape it produces is asserted
/// here, because it is the one every search in the product will take.
#[rstest]
fn test_a_free_text_query_becomes_an_or_of_three_matches() {
    let pattern = RegexLiteral::containing("gateway").expect("a short literal");
    let predicate = Predicate::Or(
        ["metadata.name", "metadata.title", "metadata.tags"]
            .into_iter()
            .map(|path| Predicate::Matches {
                field: field(path),
                pattern: pattern.clone(),
            })
            .collect(),
    );

    assert_eq!(
        predicate.to_json(),
        json!({
            "or": [
                { "metadata.name": { "matches": "/gateway/i" } },
                { "metadata.title": { "matches": "/gateway/i" } },
                { "metadata.tags": { "matches": "/gateway/i" } },
            ]
        })
    );
}

/// Labels and fields become `eq`, which is the other half of a search.
#[rstest]
fn test_labels_and_fields_become_eq() {
    let predicate = Predicate::And(vec![
        eq("metadata.labels.environment", "demo"),
        eq("spec.tier", "backend"),
    ]);

    assert_eq!(
        predicate.to_json(),
        json!({
            "and": [
                { "metadata.labels.environment": { "eq": "demo" } },
                { "spec.tier": { "eq": "backend" } },
            ]
        })
    );
}

// ---------------------------------------------------------------------------------------------
// Field validation — anything the engine would reject is unconstructible.
// ---------------------------------------------------------------------------------------------

#[rstest]
#[case("apiVersion")]
#[case("kind")]
#[case("metadata.name")]
#[case("metadata.title")]
#[case("metadata.tags")]
#[case("metadata.urn")]
#[case("metadata.labels.environment")]
#[case("spec.replicas")]
#[case("spec.container.image")]
fn test_accepted_fields(#[case] path: &str) {
    assert!(FieldPath::new(path).is_ok());
}

#[rstest]
#[case::unknown_root("status")]
#[case::metadata_not_filterable("metadata.description")]
#[case::bare_labels("metadata.labels.")]
#[case::bare_spec("spec.")]
#[case::empty("")]
#[case::injection("metadata.name\": {\"eq\": 1}, \"kind")]
fn test_refused_fields(#[case] path: &str) {
    let error = FieldPath::new(path).expect_err("an unfilterable field is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
}

// ---------------------------------------------------------------------------------------------
// Regex literals — metacharacters, quotes and non-ASCII.
// ---------------------------------------------------------------------------------------------

/// A search for `a.b` must not become a wildcard, and a search for `(` must not become a parse
/// error the model has to understand.
#[rstest]
#[case::dot("a.b", r"/a\.b/i")]
#[case::star("a*", r"/a\*/i")]
#[case::open_paren("(", r"/\(/i")]
#[case::bracket("[a-z]", r"/\[a\-z\]/i")]
#[case::anchor("^end$", r"/\^end\$/i")]
#[case::backslash(r"a\b", r"/a\\b/i")]
fn test_metacharacters_are_escaped(#[case] text: &str, #[case] expected: &str) {
    assert_eq!(
        RegexLiteral::containing(text)
            .expect("an escapable literal")
            .as_str(),
        expected
    );
}

/// A quote inside the pattern must survive JSON encoding without breaking the object.
#[rstest]
fn test_quotes_survive_encoding() {
    let predicate = Predicate::Matches {
        field: field("metadata.title"),
        pattern: RegexLiteral::containing(r#"say "hello""#).expect("an escapable literal"),
    };

    let encoded = predicate.encode_rawq().expect("a small query encodes");
    let decoded = decode(&encoded[0]);

    // `regex::escape` leaves a double quote alone — it is not a metacharacter — so the literal
    // carries it as-is and JSON encoding is what makes it safe on the wire.
    assert_eq!(
        decoded["metadata.title"]["matches"],
        json!(r#"/say "hello"/i"#)
    );
}

#[rstest]
#[case::accents("caffè")]
#[case::cjk("サービス")]
#[case::emoji("🚀")]
fn test_non_ascii_survives_encoding(#[case] text: &str) {
    let predicate = Predicate::Matches {
        field: field("metadata.name"),
        pattern: RegexLiteral::containing(text).expect("an escapable literal"),
    };

    let encoded = predicate.encode_rawq().expect("a small query encodes");
    let decoded = decode(&encoded[0]);

    assert!(
        decoded["metadata.name"]["matches"]
            .as_str()
            .expect("a string literal")
            .contains(text)
    );
}

/// The engine caps a `matches` literal at 256 bytes, and the escaping can double a length — so
/// the check is on the escaped literal, not on the user's text.
#[rstest]
fn test_an_over_long_pattern_is_refused() {
    let error = RegexLiteral::containing(&"a".repeat(MAX_REGEX_BYTES))
        .expect_err("an over-long pattern is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert!(error.message.contains(&MAX_REGEX_BYTES.to_string()));
}

#[rstest]
fn test_a_pattern_at_the_boundary_is_accepted() {
    // `/` + text + `/i` is three bytes of wrapper.
    let text = "a".repeat(MAX_REGEX_BYTES - 3);

    assert!(RegexLiteral::containing(&text).is_ok());
}

// ---------------------------------------------------------------------------------------------
// Value limits — refused, never silently trimmed.
// ---------------------------------------------------------------------------------------------

#[rstest]
fn test_a_value_at_the_boundary_is_accepted() {
    assert!(QueryValue::string(&"a".repeat(MAX_VALUE_BYTES)).is_ok());
}

#[rstest]
fn test_an_over_long_value_is_refused_rather_than_trimmed() {
    let error = QueryValue::string(&"a".repeat(MAX_VALUE_BYTES + 1))
        .expect_err("an over-long value is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
}

/// The message is cut at a character boundary, so a multi-byte value does not produce a panic
/// or a mojibake error.
#[rstest]
fn test_an_over_long_multibyte_value_reports_readably() {
    let error = QueryValue::string(&"é".repeat(MAX_VALUE_BYTES))
        .expect_err("an over-long value is refused");

    assert!(error.message.contains('é'));
}

// ---------------------------------------------------------------------------------------------
// The declared limits — including the three the engine does not enforce yet.
// ---------------------------------------------------------------------------------------------

/// Fifty leaves, arranged so that the branch and depth limits cannot be what fails.
fn leaves(count: usize) -> Predicate {
    let groups = count.div_ceil(5);

    Predicate::And(
        (0..groups)
            .map(|group| {
                let remaining = count - group * 5;

                Predicate::Or(
                    (0..remaining.min(5))
                        .map(|index| eq("kind", &format!("Kind{group}x{index}")))
                        .collect(),
                )
            })
            .collect(),
    )
}

#[rstest]
fn test_the_leaf_limit_at_its_boundary() {
    assert!(leaves(MAX_LEAF_PREDICATES).validate().is_ok());

    let error = leaves(MAX_LEAF_PREDICATES + 1)
        .validate()
        .expect_err("one leaf over the limit is refused");

    assert_eq!(error.code, codes::QUERY_TOO_LARGE);
    assert!(
        error
            .message
            .contains(&(MAX_LEAF_PREDICATES + 1).to_string())
    );
}

#[rstest]
fn test_the_branch_limit_at_its_boundary() {
    let at = Predicate::And(
        (0..MAX_BRANCH_CHILDREN)
            .map(|index| eq("kind", &format!("Kind{index}")))
            .collect(),
    );
    let over = Predicate::And(
        (0..MAX_BRANCH_CHILDREN + 1)
            .map(|index| eq("kind", &format!("Kind{index}")))
            .collect(),
    );

    assert!(at.validate().is_ok());

    let error = over.validate().expect_err("an over-wide group is refused");
    assert_eq!(error.code, codes::QUERY_TOO_LARGE);
}

#[rstest]
fn test_the_depth_limit_at_its_boundary() {
    /// Nests `depth` levels of `and` around one leaf.
    fn nest(depth: usize) -> Predicate {
        let mut predicate = eq("kind", "Service");

        for _ in 1..depth {
            predicate = Predicate::And(vec![predicate]);
        }

        predicate
    }

    assert!(nest(MAX_DEPTH).validate().is_ok());

    let error = nest(MAX_DEPTH + 1)
        .validate()
        .expect_err("an over-deep query is refused");
    assert_eq!(error.code, codes::QUERY_TOO_LARGE);
}

#[rstest]
fn test_an_empty_group_is_refused() {
    let error = Predicate::And(vec![])
        .validate()
        .expect_err("an empty group is refused");

    assert_eq!(error.code, codes::INVALID_INPUT);
}

// ---------------------------------------------------------------------------------------------
// Encoding, the golden, and the split.
// ---------------------------------------------------------------------------------------------

/// The golden: a fixed query encodes to exactly these bytes, on every platform and every run.
#[rstest]
fn test_the_golden_base64_for_a_fixed_query() {
    let predicate = Predicate::And(vec![
        eq("kind", "Service"),
        Predicate::Matches {
            field: field("metadata.name"),
            pattern: RegexLiteral::containing("gateway").expect("a short literal"),
        },
    ]);

    let encoded = predicate.encode_rawq().expect("a small query encodes");

    assert_eq!(encoded.len(), 1);
    assert_eq!(
        encoded[0],
        "eyJhbmQiOlt7ImtpbmQiOnsiZXEiOiJTZXJ2aWNlIn19LHsibWV0YWRhdGEubmFtZSI6eyJtYXRjaGVzIjoiL2dhdGV3YXkvaSJ9fV19"
    );
    // ...and the engine can read it back.
    assert_eq!(
        decode(&encoded[0]),
        json!({
            "and": [
                { "kind": { "eq": "Service" } },
                { "metadata.name": { "matches": "/gateway/i" } },
            ]
        })
    );
}

/// The compliance `raw-query` scope carries the query object **directly**: the same AST, no
/// base64 anywhere. Reusing the AST and not the encoding is the correction to T4's analysis.
#[rstest]
fn test_the_compliance_scope_is_plain_json() {
    let predicate = eq("kind", "Service");

    assert_eq!(
        predicate
            .to_raw_query_scope()
            .expect("a small query serialises"),
        json!({ "kind": { "eq": "Service" } })
    );
}

/// A query that fits stays in one parameter.
#[rstest]
fn test_a_small_query_is_one_parameter() {
    let encoded = eq("kind", "Service")
        .encode_rawq()
        .expect("a small query encodes");

    assert_eq!(encoded.len(), 1);
    assert!(encoded[0].len() <= MAX_RAWQ_PARAM_BYTES);
}

/// An `and` of `count` maximally-sized leaves. `count` must stay inside the branch limit.
fn and_of_wide_leaves(count: usize) -> Predicate {
    Predicate::And(
        (0..count)
            .map(|index| {
                let value = format!("{index:04}{}", "x".repeat(MAX_VALUE_BYTES - 4));

                Predicate::Eq {
                    field: field("spec.tier"),
                    value: QueryValue::string(&value).expect("a value at the cap"),
                }
            })
            .collect(),
    )
}

/// The encoded length of a whole query as one parameter.
fn encoded_len(predicate: &Predicate) -> usize {
    URL_SAFE_NO_PAD
        .encode(predicate.to_json().to_string())
        .len()
}

/// A query at, and either side of, the per-parameter cap.
#[rstest]
fn test_the_per_parameter_cap_decides_whether_to_split() {
    let under = and_of_wide_leaves(7);
    assert!(
        encoded_len(&under) < MAX_RAWQ_PARAM_BYTES,
        "the fixture is meant to sit under the cap, and is {} bytes",
        encoded_len(&under)
    );
    assert_eq!(
        under
            .encode_rawq()
            .expect("a query under the cap encodes")
            .len(),
        1,
        "a query under the cap must not be split"
    );

    let over = and_of_wide_leaves(10);
    assert!(
        encoded_len(&over) > MAX_RAWQ_PARAM_BYTES,
        "the fixture is meant to sit over the cap, and is {} bytes",
        encoded_len(&over)
    );
    let split = over.encode_rawq().expect("a query over the cap splits");
    assert!(split.len() > 1, "a query over the cap must be split");
}

/// Every parameter of a split is inside the cap, and the split is **equivalent** to the whole:
/// the engine AND-s repeated `rawq` parameters, so the conditions are the same conditions.
#[rstest]
fn test_a_split_is_equivalent_to_the_whole_query() {
    let predicate = and_of_wide_leaves(10);
    let original = predicate.to_json();

    let parameters = predicate.encode_rawq().expect("the query splits");

    let mut recombined: Vec<Value> = Vec::new();
    for parameter in &parameters {
        assert!(
            parameter.len() <= MAX_RAWQ_PARAM_BYTES,
            "a split parameter is still over the cap"
        );

        let decoded = decode(parameter);
        recombined.extend(
            decoded["and"]
                .as_array()
                .expect("each parameter is an `and`")
                .clone(),
        );
    }

    assert_eq!(
        Value::Array(recombined),
        original["and"],
        "the split changed which conditions are asked for"
    );
}

/// **The 8 KiB total is the binding constraint, not the four-parameter count.**
///
/// Four parameters of 5 600 bytes would be 22 400, so the total cap is reached first in every
/// realistic shape. Worth pinning: a reader of §8.8 could reasonably expect `MAX_RAWQ_PARAMS` to
/// be what fires, and it is not.
#[rstest]
fn test_the_total_query_string_cap_binds_before_the_parameter_count() {
    let predicate = and_of_wide_leaves(13);

    let error = predicate
        .encode_rawq()
        .expect_err("a query over the total cap is refused");

    assert_eq!(error.code, codes::QUERY_TOO_LARGE);
    assert!(
        error.details.expect("the budget is reported")["parametersNeeded"]
            .as_u64()
            .expect("a count")
            <= MAX_RAWQ_PARAMS as u64,
        "the parameter count was not what failed"
    );
}

/// A query no legal split can carry is a tool error naming what dominated — not a `400` from
/// the engine the model cannot interpret.
///
/// Note the shape: forty leaves in twenty groups is inside every *declared* limit — fifty
/// leaves, twenty per group, six deep — and still too big to encode. That is exactly why the
/// byte budget is a separate check rather than something the limits imply.
#[rstest]
fn test_a_query_too_large_to_split_is_refused() {
    let predicate = Predicate::And(
        (0..MAX_BRANCH_CHILDREN)
            .map(|group| {
                Predicate::Or(
                    (0..2)
                        .map(|index| {
                            let value =
                                format!("{group:02}{index:02}{}", "x".repeat(MAX_VALUE_BYTES - 4));

                            Predicate::Eq {
                                field: field("spec.tier"),
                                value: QueryValue::string(&value).expect("a value at the cap"),
                            }
                        })
                        .collect(),
                )
            })
            .collect(),
    );

    let error = predicate
        .encode_rawq()
        .expect_err("a query beyond the split budget is refused");

    assert_eq!(error.code, codes::QUERY_TOO_LARGE);
    assert_eq!(error.remedy, Remedy::RetryAfterChange);
    assert_eq!(
        error.next_step.as_deref(),
        Some("narrow the search — fewer conditions, or shorter values")
    );
    assert_eq!(
        error.details.expect("the budget is reported")["maxParameters"],
        json!(MAX_RAWQ_PARAMS)
    );
}

/// A single condition too big for one parameter cannot be split at all, and says so rather than
/// producing a parameter the engine will reject.
#[rstest]
fn test_a_single_oversized_condition_cannot_be_split() {
    // The value cap keeps one `eq` small, so an `or` is the way to build one oversized child.
    let wide = Predicate::Or(
        (0..MAX_BRANCH_CHILDREN)
            .map(|index| {
                let value = format!("{index:04}{}", "x".repeat(MAX_VALUE_BYTES - 4));

                Predicate::Eq {
                    field: field("spec.tier"),
                    value: QueryValue::string(&value).expect("a value at the cap"),
                }
            })
            .collect(),
    );
    let predicate = Predicate::And(vec![wide]);

    let error = predicate
        .encode_rawq()
        .expect_err("an unsplittable condition is refused");

    assert_eq!(error.code, codes::QUERY_TOO_LARGE);
}

/// Encoding validates first: an invalid query never becomes bytes.
#[rstest]
fn test_encoding_validates_before_encoding() {
    let error = Predicate::And(vec![])
        .encode_rawq()
        .expect_err("an invalid query does not encode");

    assert_eq!(error.code, codes::INVALID_INPUT);
}
