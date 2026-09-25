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
use crate::error::{Remedy, ToolError, codes};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::LazyLock;

/// Maximum bytes in one `matches` regex literal. The engine's own cap.
pub const MAX_REGEX_BYTES: usize = 256;

/// Maximum bytes in one string value. The engine's own cap.
pub const MAX_VALUE_BYTES: usize = 512;

/// Maximum bytes in one **encoded** `rawq` parameter. The engine measures the base64, not the
/// JSON, so this is checked after encoding and not before.
pub const MAX_RAWQ_PARAM_BYTES: usize = 5_600;

/// Maximum number of `rawq` parameters we will split a query across. The engine AND-s repeated
/// `rawq` parameters, which is what makes splitting equivalent to not splitting.
pub const MAX_RAWQ_PARAMS: usize = 4;

/// Maximum total bytes of `rawq` parameters, so a split cannot produce a request line no
/// intermediary will carry.
pub const MAX_QUERY_STRING_BYTES: usize = 8_192;

/// Maximum leaf predicates in one query. **The engine declares this and does not enforce it
/// yet** — a tool that works only because a check is missing is a defect waiting for somebody
/// else's commit, so we enforce it here.
pub const MAX_LEAF_PREDICATES: usize = 50;

/// Maximum children inside one `and` or `or`. Declared by the engine, unenforced there.
pub const MAX_BRANCH_CHILDREN: usize = 20;

/// Maximum operator nesting depth. Declared by the engine, unenforced there.
pub const MAX_DEPTH: usize = 6;

/// The engine's own `matches` literal pattern: `/pattern/` or `/pattern/i`.
static MATCHES_LITERAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern, copied from the engine.
    Regex::new(r"^/((?:[^/\\]|\\.)*)/(i)?$").expect("MATCHES_LITERAL_RE is a constant pattern")
});

/// The engine's label-key grammar (`LABEL_ANNOTATION_KEY_PATTERN`), anchored at **both** ends.
///
/// The engine anchors only the start; this is stricter on purpose. A label key becomes part of a
/// field path — `metadata.labels.<key>`, everything after the prefix, dots and slash included —
/// so an unvalidated key would be a way to smuggle a different path into a query.
static LABEL_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern, copied from the engine.
    Regex::new(r"^([a-zA-Z0-9][a-zA-Z0-9.-]{0,253}/)?[a-zA-Z0-9][a-zA-Z0-9.-]{0,63}[a-zA-Z0-9]?$")
        .expect("LABEL_KEY_RE is a constant pattern")
});

/// Whether `key` is a label key the engine accepts — for a tool that wants to name the parameter
/// in its error before [`FieldPath::new`] would refuse the path.
pub fn is_valid_label_key(key: &str) -> bool {
    LABEL_KEY_RE.is_match(key)
}

/// A field the engine will accept in a query (§8.8).
///
/// **Anything else is unconstructible.** The engine's parser rejects an unknown field with a
/// `400`, and a `400` on a parameter we built is a `server_defect` the model cannot act on — so
/// the validation happens here, where it can still be an `invalid_input` the model can.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldPath(String);

impl FieldPath {
    /// Validates a field path against the engine's accepted set.
    pub fn new(path: &str) -> Result<Self, ToolError> {
        let accepted = matches!(
            path,
            "apiVersion"
                | "kind"
                | "metadata.name"
                | "metadata.title"
                | "metadata.tags"
                | "metadata.urn"
        ) || path
            .strip_prefix("metadata.labels.")
            .is_some_and(is_valid_label_key)
            || path
                .strip_prefix("spec.")
                .is_some_and(|rest| !rest.is_empty());

        if !accepted {
            return Err(ToolError::new(
                codes::INVALID_INPUT,
                Remedy::RetryAfterChange,
                format!("`{path}` is not a field the catalog can filter on."),
            )
            .with_details(json!({
                "field": path,
                "validFields": [
                    "apiVersion", "kind", "metadata.name", "metadata.title", "metadata.tags",
                    "metadata.urn", "metadata.labels.<key>", "spec.<path>",
                ],
            })));
        }

        Ok(Self(path.to_string()))
    }

    /// The path, as the engine spells it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A `matches` value: a regex **literal**, not a bare string (§8.8).
///
/// Built by escaping the user's text, so a search for `a.b` cannot become a wildcard and a
/// search for `(` cannot become a parse error the model has to understand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexLiteral(String);

impl RegexLiteral {
    /// Escapes `text` and wraps it as a case-insensitive literal.
    ///
    /// The result is asserted to compile before it leaves: a pattern the engine cannot parse is
    /// a `400` on a parameter we built, which is the one kind of failure the model cannot fix.
    pub fn containing(text: &str) -> Result<Self, ToolError> {
        let literal = format!("/{}/i", regex::escape(text));

        if literal.len() > MAX_REGEX_BYTES {
            return Err(ToolError::new(
                codes::INVALID_INPUT,
                Remedy::RetryAfterChange,
                format!(
                    "The search text is too long: it becomes a {} byte pattern and the catalog \
                     accepts at most {MAX_REGEX_BYTES}.",
                    literal.len()
                ),
            )
            .with_details(json!({ "bytes": literal.len(), "maxBytes": MAX_REGEX_BYTES }))
            .with_next_step("search for a shorter phrase"));
        }

        let inner = MATCHES_LITERAL_RE
            .captures(&literal)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| server_defect_pattern(&literal))?;

        Regex::new(&inner).map_err(|_| server_defect_pattern(&literal))?;

        Ok(Self(literal))
    }

    /// The literal, as the engine expects it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An escaped literal that does not compile is a defect of ours, not of the caller's text.
fn server_defect_pattern(literal: &str) -> ToolError {
    tracing::error!(
        pattern = literal,
        "the translator built an unusable pattern"
    );

    ToolError::new(
        codes::SERVER_DEFECT,
        Remedy::Escalate,
        "This server built a search pattern the catalog cannot read.",
    )
}

/// A value on the right of a comparison.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryValue {
    /// A string, capped at [`MAX_VALUE_BYTES`].
    String(String),
    /// A whole number.
    Integer(i64),
    /// A fractional number.
    Number(f64),
    /// A boolean.
    Boolean(bool),
    /// An explicit null, which the engine treats as a value rather than as absence.
    Null,
}

impl QueryValue {
    /// Builds a string value, refusing one the engine would reject.
    ///
    /// **Refused, not trimmed.** A silently shortened value would match different objects than
    /// the caller asked for, and they would have no way to tell — the same failure this plan
    /// removes from response sizes.
    pub fn string(text: &str) -> Result<Self, ToolError> {
        if text.len() > MAX_VALUE_BYTES {
            // Reported at a character boundary so the message is readable even when the value is
            // cut mid-codepoint by byte count.
            let shown = &text[..floor_char_boundary(text, 32)];

            return Err(ToolError::new(
                codes::INVALID_INPUT,
                Remedy::RetryAfterChange,
                format!(
                    "The value starting `{shown}…` is {} bytes and the catalog accepts at most \
                     {MAX_VALUE_BYTES}.",
                    text.len()
                ),
            )
            .with_details(json!({ "bytes": text.len(), "maxBytes": MAX_VALUE_BYTES })));
        }

        Ok(Self::String(text.to_string()))
    }

    /// The JSON the engine reads.
    fn to_json(&self) -> Value {
        match self {
            Self::String(value) => json!(value),
            Self::Integer(value) => json!(value),
            Self::Number(value) => json!(value),
            Self::Boolean(value) => json!(value),
            Self::Null => Value::Null,
        }
    }
}

/// The largest index `<= max` that sits on a character boundary.
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut index = max.min(text.len());

    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }

    index
}

/// **Only what the tool set emits.** Adding a variant is a deliberate act (§8.8).
///
/// The engine understands ten operators; four are enough for every tool in this plan, and each
/// one we do not emit is one whose semantics we do not have to explain to a model.
#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    /// `{"<field>": {"eq": <value>}}`
    Eq {
        /// The field to compare.
        field: FieldPath,
        /// What to compare it to.
        value: QueryValue,
    },

    /// `{"<field>": {"matches": "/pattern/i"}}`
    Matches {
        /// The field to match.
        field: FieldPath,
        /// The escaped literal.
        pattern: RegexLiteral,
    },

    /// `{"and": [...]}` — every child must hold.
    And(Vec<Predicate>),

    /// `{"or": [...]}` — at least one child must hold.
    Or(Vec<Predicate>),
}

impl Predicate {
    /// The engine's own JSON shape.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Eq { field, value } => {
                let mut operator = Map::new();
                operator.insert("eq".to_string(), value.to_json());

                one_key(field.as_str(), Value::Object(operator))
            }
            Self::Matches { field, pattern } => {
                let mut operator = Map::new();
                operator.insert("matches".to_string(), json!(pattern.as_str()));

                one_key(field.as_str(), Value::Object(operator))
            }
            Self::And(children) => {
                json!({ "and": children.iter().map(Self::to_json).collect::<Vec<_>>() })
            }
            Self::Or(children) => {
                json!({ "or": children.iter().map(Self::to_json).collect::<Vec<_>>() })
            }
        }
    }

    /// Checks the limits the engine declares, including the three it does not enforce yet.
    pub fn validate(&self) -> Result<(), ToolError> {
        let leaves = self.count_leaves();

        if leaves > MAX_LEAF_PREDICATES {
            return Err(too_large(format!(
                "The search has {leaves} conditions and the catalog accepts at most \
                 {MAX_LEAF_PREDICATES}."
            )));
        }

        self.validate_node(1)
    }

    /// Per-node checks: branch width and nesting depth.
    fn validate_node(&self, depth: usize) -> Result<(), ToolError> {
        if depth > MAX_DEPTH {
            return Err(too_large(format!(
                "The search nests more than {MAX_DEPTH} levels deep."
            )));
        }

        match self {
            Self::Eq { .. } | Self::Matches { .. } => Ok(()),
            Self::And(children) | Self::Or(children) => {
                if children.is_empty() {
                    return Err(ToolError::new(
                        codes::INVALID_INPUT,
                        Remedy::RetryAfterChange,
                        "A search group must contain at least one condition.",
                    ));
                }

                if children.len() > MAX_BRANCH_CHILDREN {
                    return Err(too_large(format!(
                        "A search group has {} conditions and the catalog accepts at most \
                         {MAX_BRANCH_CHILDREN}.",
                        children.len()
                    )));
                }

                children
                    .iter()
                    .try_for_each(|child| child.validate_node(depth + 1))
            }
        }
    }

    /// How many leaf predicates the tree carries.
    fn count_leaves(&self) -> usize {
        match self {
            Self::Eq { .. } | Self::Matches { .. } => 1,
            Self::And(children) | Self::Or(children) => {
                children.iter().map(Self::count_leaves).sum()
            }
        }
    }

    /// Encodes the query for the `rawq` query parameter, splitting when it has to (§8.8).
    ///
    /// The engine AND-s repeated `rawq` parameters, so splitting a top-level `and` across
    /// several of them is **equivalent** to sending one — which is the only reason splitting is
    /// allowed to be invisible to the caller. Anything that cannot be split that way, or that
    /// would need more than [`MAX_RAWQ_PARAMS`], is a tool error asking for a narrower search
    /// rather than a `400` from the engine.
    pub fn encode_rawq(&self) -> Result<Vec<String>, ToolError> {
        self.validate()?;

        let whole = encode_one(&self.to_json());

        if whole.len() <= MAX_RAWQ_PARAM_BYTES {
            return Ok(vec![whole]);
        }

        let Self::And(children) = self else {
            return Err(too_large_for_split(whole.len(), 1));
        };

        let mut parameters: Vec<String> = Vec::new();
        let mut group: Vec<Value> = Vec::new();

        for child in children {
            let mut candidate = group.clone();
            candidate.push(child.to_json());

            if encode_one(&and_of(&candidate)).len() <= MAX_RAWQ_PARAM_BYTES {
                group = candidate;
                continue;
            }

            if group.is_empty() {
                // One child alone does not fit; no split can help.
                return Err(too_large_for_split(
                    encode_one(&child.to_json()).len(),
                    parameters.len() + 1,
                ));
            }

            parameters.push(encode_one(&and_of(&group)));
            group = vec![child.to_json()];
        }

        if !group.is_empty() {
            parameters.push(encode_one(&and_of(&group)));
        }

        if parameters.len() > MAX_RAWQ_PARAMS {
            return Err(too_large_for_split(whole.len(), parameters.len()));
        }

        let total: usize = parameters.iter().map(String::len).sum();
        if total > MAX_QUERY_STRING_BYTES {
            return Err(too_large_for_split(total, parameters.len()));
        }

        Ok(parameters)
    }

    /// The **plain JSON** form a compliance `raw-query` scope carries.
    ///
    /// The same AST, a different serialisation: that body takes the query object directly, with
    /// no base64 anywhere. Reusing the AST and not the encoding is the correction the plan makes
    /// to T4's analysis.
    pub fn to_raw_query_scope(&self) -> Result<Value, ToolError> {
        self.validate()?;

        Ok(self.to_json())
    }
}

/// `{"and": [...]}` around already-serialised children.
fn and_of(children: &[Value]) -> Value {
    json!({ "and": children })
}

/// Compact JSON, then URL-safe base64 without padding — what the engine decodes.
fn encode_one(query: &Value) -> String {
    // The **decoded** object is what gets logged; the base64 never is, because a base64 blob in
    // a log is the thing that makes a translator undebuggable.
    tracing::debug!(query = %query, "encoded a rawq parameter");

    URL_SAFE_NO_PAD.encode(query.to_string())
}

/// The error for a query that exceeds a declared limit.
fn too_large(message: String) -> ToolError {
    ToolError::new(codes::QUERY_TOO_LARGE, Remedy::RetryAfterChange, message)
        .with_next_step("narrow the search and try again")
}

/// The error for a query no legal split can carry, naming what dominated.
fn too_large_for_split(bytes: usize, parameters: usize) -> ToolError {
    ToolError::new(
        codes::QUERY_TOO_LARGE,
        Remedy::RetryAfterChange,
        format!(
            "The search is {bytes} bytes once encoded, which does not fit in the \
             {MAX_RAWQ_PARAMS} query parameters the catalog accepts."
        ),
    )
    .with_details(json!({
        "encodedBytes": bytes,
        "parametersNeeded": parameters,
        "maxParameters": MAX_RAWQ_PARAMS,
        "maxBytesPerParameter": MAX_RAWQ_PARAM_BYTES,
    }))
    .with_next_step("narrow the search — fewer conditions, or shorter values")
}

/// A single-key object, which is the shape every field predicate takes.
fn one_key(key: &str, value: Value) -> Value {
    let mut object = Map::new();
    object.insert(key.to_string(), value);

    Value::Object(object)
}

#[cfg(test)]
mod tests;
