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
use http::HeaderMap;
use regex::Regex;
use std::sync::LazyLock;

/// The engine emits one `Warning` header per warning, value exactly `299 - "<message>"`, and the
/// header is repeatable.
static WARNING_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r#"^(?<code>\d{3}) - "(?<text>.*)"$"#).expect("WARNING_RE is a constant pattern")
});

/// The read-only-field warning the engine emits from an Item Type Definition update, from which
/// T12 derives its `ignored` list (D28).
pub static READ_ONLY_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r"^'(?<field>[^']+)' field is read-only and was ignored during the update\.$")
        .expect("READ_ONLY_FIELD_RE is a constant pattern")
});

/// One `Warning: 299 - "…"` the engine attached to a response (P6, D28).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineWarning {
    /// The warn-code. Always `299` from this engine, parsed rather than assumed.
    pub code: u16,

    /// The message, unescaped of its surrounding quotes and nothing else.
    pub text: String,
}

impl EngineWarning {
    /// The field name, when this warning is the read-only-field one T12 reads.
    pub fn read_only_field(&self) -> Option<&str> {
        READ_ONLY_FIELD_RE
            .captures(&self.text)
            .and_then(|caps| caps.name("field"))
            .map(|m| m.as_str())
    }
}

impl std::fmt::Display for EngineWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} - \"{}\"", self.code, self.text)
    }
}

/// Collects **all** `Warning` headers from one engine response (P6).
///
/// One parser, one place. A value that does not match the engine's shape is dropped rather than
/// guessed at: a malformed warning is not worth failing a good response over, and surfacing a
/// half-parsed one to the model would be worse than surfacing none.
pub fn parse(headers: &HeaderMap) -> Vec<EngineWarning> {
    headers
        .get_all(http::header::WARNING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(parse_one)
        .collect()
}

/// Parses one `Warning` header value.
fn parse_one(raw: &str) -> Option<EngineWarning> {
    let caps = WARNING_RE.captures(raw)?;

    Some(EngineWarning {
        code: caps.name("code")?.as_str().parse().ok()?,
        text: caps.name("text")?.as_str().to_string(),
    })
}

#[cfg(test)]
mod tests;
