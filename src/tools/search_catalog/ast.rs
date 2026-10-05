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
// The parameters → `Predicate` mapping, which is this tool's **entire** contribution to the query
// translator. Encoding, limits, splitting and base64 are `catalog-client`'s.

use catalog_client::{
    FieldPath, Predicate, QueryValue, RegexLiteral, ToolError, query::MAX_BRANCH_CHILDREN,
};
use std::collections::BTreeMap;

/// The fields free text is matched against, in the order they are emitted.
///
/// `metadata.tags` is an array, and the engine matches a pattern against **any element** of it
/// (`EXISTS … unnest(…) … ~*`) — which is what lets one `matches` cover a tag list.
const QUERY_FIELDS: [&str; 3] = ["metadata.name", "metadata.title", "metadata.tags"];

/// The `fields` argument: a path, and the value it must equal — or `None`, for a field that must
/// be unset.
pub type FieldFilters = BTreeMap<String, Option<String>>;

/// The field-path prefix a label key is appended to.
pub const LABEL_PATH_PREFIX: &str = "metadata.labels.";

/// Builds the search's predicate, or `None` when nothing was asked for.
///
/// - `query` → an `or` of three `matches`, each with the **same** literal: `regex::escape`d and
///   wrapped `/…/i`, so the semantics are a case-insensitive substring, which is what a person
///   means by "search".
/// - each label → `eq` on `metadata.labels.<key>`; each field → `eq` on its path, or, for a
///   `null` value, `exists: false` on it.
/// - everything is `and`-ed at the top level, which is also the only shape `catalog-client` may
///   split across several `rawq` parameters.
/// - **when that `and` would be wider than `catalog-client`'s [`MAX_BRANCH_CHILDREN`]**, the `eq`s
///   are grouped into nested `and`s of at most that many, after the query's `or`. `query` plus 20
///   labels plus 20 fields — all within the tool's own bounds — would otherwise be refused for
///   width. A conjunction of conjunctions is the same search, and the top-level `and` stays
///   splittable. A search that fits keeps exactly the shape it always had, so its `rawq` and its
///   cursor fingerprint do not change.
///
/// **Nothing supplied is not an error**: it yields `None`, and no `rawq` is sent at all — *"what
/// is in the catalog?"* is a legitimate first question. `kind` is never a predicate: it selects
/// the endpoint, and repeating it in every `rawq` would only cost bytes.
///
/// `BTreeMap` iteration makes the result independent of the order the model wrote its keys in,
/// so two identical searches produce byte-identical `rawq` and the same cursor fingerprint.
pub fn build(
    query: Option<&str>,
    labels: Option<&BTreeMap<String, String>>,
    fields: Option<&FieldFilters>,
) -> Result<Option<Predicate>, ToolError> {
    let mut clauses = Vec::new();
    let mut equalities = Vec::new();

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
        equalities.push(Predicate::Eq {
            field: FieldPath::new(&format!("{LABEL_PATH_PREFIX}{key}"))?,
            value: QueryValue::string(value)?,
        });
    }

    for (path, value) in fields.into_iter().flatten() {
        let field = FieldPath::new(path)?;
        equalities.push(match value {
            Some(value) => Predicate::Eq {
                field,
                value: QueryValue::string(value)?,
            },
            None => Predicate::Missing { field },
        });
    }

    if clauses.len() + equalities.len() <= MAX_BRANCH_CHILDREN {
        clauses.extend(equalities);
    } else {
        clauses.extend(
            equalities
                .chunks(MAX_BRANCH_CHILDREN)
                .map(|group| match group {
                    [only] => only.clone(),
                    _ => Predicate::And(group.to_vec()),
                }),
        );
    }

    Ok((!clauses.is_empty()).then_some(Predicate::And(clauses)))
}
