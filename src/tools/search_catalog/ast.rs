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
// T2 §4 — the parameters → `Predicate` mapping, which is T2's **entire** contribution to the
// query translator. Encoding, limits, splitting and base64 are the core's (§8.8).

use catalog_client::{FieldPath, Predicate, QueryValue, RegexLiteral, ToolError};
use std::collections::BTreeMap;

/// The fields free text is matched against, in the order they are emitted.
///
/// `metadata.tags` is an array, and the engine matches a pattern against **any element** of it
/// (`EXISTS … unnest(…) … ~*`, T2-P2) — which is what lets one `matches` cover a tag list.
const QUERY_FIELDS: [&str; 3] = ["metadata.name", "metadata.title", "metadata.tags"];

/// The field-path prefix a label key is appended to.
pub const LABEL_PATH_PREFIX: &str = "metadata.labels.";

/// Builds the search's predicate, or `None` when nothing was asked for.
///
/// - `query` → an `or` of three `matches`, each with the **same** literal: `regex::escape`d and
///   wrapped `/…/i`, so the semantics are a case-insensitive substring, which is what a person
///   means by "search".
/// - each label → `eq` on `metadata.labels.<key>`; each field → `eq` on its path.
/// - everything is `and`-ed at the top level, which is also the only shape the core may split
///   across several `rawq` parameters.
///
/// **Nothing supplied is not an error**: it yields `None`, and no `rawq` is sent at all — *"what
/// is in the catalog?"* is a legitimate first question. `kind` is never a predicate: it selects
/// the endpoint (T2-D2), and repeating it in every `rawq` would only cost bytes.
///
/// `BTreeMap` iteration makes the result independent of the order the model wrote its keys in,
/// so two identical searches produce byte-identical `rawq` and the same cursor fingerprint.
pub fn build(
    query: Option<&str>,
    labels: Option<&BTreeMap<String, String>>,
    fields: Option<&BTreeMap<String, String>>,
) -> Result<Option<Predicate>, ToolError> {
    let mut clauses = Vec::new();

    if let Some(query) = query {
        let pattern = RegexLiteral::containing(query)?;
        let matches = QUERY_FIELDS
            .into_iter()
            .map(|field| {
                Ok(Predicate::Matches {
                    field: FieldPath::new(field)?,
                    pattern: pattern.clone(),
                })
            })
            .collect::<Result<Vec<_>, ToolError>>()?;

        clauses.push(Predicate::Or(matches));
    }

    for (key, value) in labels.into_iter().flatten() {
        clauses.push(Predicate::Eq {
            field: FieldPath::new(&format!("{LABEL_PATH_PREFIX}{key}"))?,
            value: QueryValue::string(value)?,
        });
    }

    for (path, value) in fields.into_iter().flatten() {
        clauses.push(Predicate::Eq {
            field: FieldPath::new(path)?,
            value: QueryValue::string(value)?,
        });
    }

    Ok((!clauses.is_empty()).then_some(Predicate::And(clauses)))
}
