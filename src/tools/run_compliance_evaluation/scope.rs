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
// A rule's scope, as the request body the engine takes: a View, checked before anything runs, or
// `search_catalog`'s filters — the same filters, the same checks and the same errors — sent as a
// plain-JSON `raw-query`.

use super::ComplianceScope;
use crate::tools::{
    compliance::parse_item_urn,
    filters::{self, Filters},
    lookup::no_such_item,
};
use catalog_client::{
    EngineClient, FieldPath, ItemAddress, Predicate, QueryValue, ToolError, error::codes,
    is_valid_name,
};
use serde_json::{Value, json};

/// The group, version and family Views live in.
const VIEW_GROUP: &str = "mia-platform.eu";
const VIEW_VERSION: &str = "v1";
const VIEW_FAMILY: &str = "views";
const VIEW_KIND: &str = "View";

/// The scheme a View URN starts with.
const URN_PREFIX: &str = "urn:mia-platform-catalog:";

/// A scope ready to send, and whether it deserves `search_catalog`'s kind-less warning.
pub struct BuiltScope {
    /// `{"scope": …}`, the evaluate request's body.
    pub body: Value,

    /// Whether a `spec.` filter was given without `kind`.
    pub spec_without_kind: bool,
}

/// Checks the scope's own shape: one form, not both, and not neither.
pub fn validate(scope: &ComplianceScope) -> Result<(), ToolError> {
    let has_filter = scope.kind.is_some()
        || scope.group.is_some()
        || scope.labels.is_some()
        || scope.fields.is_some()
        || scope.query.is_some();

    match (&scope.view, has_filter) {
        (Some(_), true) => Err(filters::invalid(
            "scope",
            "Give `view` alone, or filters without `view`: a rule takes one scope.".to_string(),
        )),
        (None, false) => Err(filters::invalid(
            "scope",
            "The scope is empty: give a `view`, or at least one of `kind`, `labels`, `fields` or \
             `query`."
                .to_string(),
        )),
        (Some(view), false) => view_name(view).map(|_| ()),
        (None, true) => filters::validate(&as_filters(scope)),
    }
}

/// Builds the evaluate body: reads the View, or resolves the type and builds the predicate.
pub async fn build(
    engine: &EngineClient,
    scope: &ComplianceScope,
) -> Result<BuiltScope, ToolError> {
    if let Some(view) = &scope.view {
        return view_scope(engine, view).await;
    }

    let mut clauses = Vec::new();

    if let Some(kind) = scope.kind.as_deref() {
        let resolved =
            filters::resolve_type(engine, kind, scope.group.as_deref(), scope.fields.as_ref())
                .await?;
        let coordinates = resolved.coordinates;

        // A compliance scope runs against the global list, where `kind` alone does not select the
        // type's endpoint as it does for a search: the type is stated in the query.
        clauses.push(Predicate::Eq {
            field: FieldPath::new("apiVersion")?,
            value: QueryValue::string(&format!("{}/{}", coordinates.group, coordinates.version))?,
        });
        clauses.push(Predicate::Eq {
            field: FieldPath::new("kind")?,
            value: QueryValue::string(&coordinates.kind)?,
        });
    }

    if let Some(predicate) = filters::build(
        scope.query.as_deref(),
        scope.labels.as_ref(),
        scope.fields.as_ref(),
    )? {
        clauses.push(predicate);
    }

    let content = Predicate::And(clauses).to_raw_query_scope()?;

    Ok(BuiltScope {
        body: json!({ "scope": { "type": "raw-query", "content": content } }),
        spec_without_kind: scope.kind.is_none() && filters::filters_on_spec(scope.fields.as_ref()),
    })
}

/// A View scope, the View read first: a wrong View is `not_found` with near matches, never an
/// evaluation that ends in a generic error.
async fn view_scope(engine: &EngineClient, view: &str) -> Result<BuiltScope, ToolError> {
    let name = view_name(view)?;
    let address = ItemAddress::new(VIEW_GROUP, VIEW_VERSION, VIEW_FAMILY, &name)?;

    let item = match engine.get_item(&address).await {
        Ok(response) => response.value,
        Err(error) if error.code == codes::NOT_FOUND => {
            return Err(no_such_item(engine, &name).await);
        }
        Err(error) => return Err(error),
    };

    let urn = item
        .metadata
        .urn
        .unwrap_or_else(|| format!("{URN_PREFIX}{VIEW_GROUP}:{VIEW_VERSION}:{VIEW_KIND}:{name}"));

    Ok(BuiltScope {
        body: json!({ "scope": { "type": "view-ref", "content": urn } }),
        spec_without_kind: false,
    })
}

/// A View's name, from its name or its URN; any other URN is refused.
fn view_name(view: &str) -> Result<String, ToolError> {
    if view.starts_with(URN_PREFIX) {
        return match parse_item_urn(view) {
            Some(parsed) if parsed.group == VIEW_GROUP && parsed.kind == VIEW_KIND => {
                Ok(parsed.name)
            }
            _ => Err(filters::invalid(
                "scope.view",
                format!("`{view}` is not a View URN; give the View's name or its URN."),
            )),
        };
    }

    if is_valid_name(view) {
        Ok(view.to_string())
    } else {
        Err(filters::invalid(
            "scope.view",
            format!("`{view}` is not a valid View name."),
        ))
    }
}

/// The scope's filters, as the shared filter rules take them.
fn as_filters(scope: &ComplianceScope) -> Filters<'_> {
    Filters {
        query: scope.query.as_deref(),
        kind: scope.kind.as_deref(),
        group: scope.group.as_deref(),
        labels: scope.labels.as_ref(),
        fields: scope.fields.as_ref(),
    }
}
