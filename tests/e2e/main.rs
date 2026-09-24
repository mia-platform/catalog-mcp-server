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
// End-to-end tests against a **live** `catalog-engine` (§12.3, §13.3).
//
// They exist for the things the OAS cannot express. `cargo make e2e` brings the environment up,
// runs them and tears it down; they are `#[ignore]`d so a plain `cargo test` — which has no
// engine — stays green.
//
// The gateway is deliberately absent from that environment (see `tests/docker-compose.yml`), so
// the engine sees exactly the headers this client forwards. That is what makes these a true test
// of D26's propagation rather than of the policy layer's regeneration of it.

use catalog_client::{
    CallerIdentity, Deadline, EngineClient, EngineClientFactory, ItemAddress,
    error::codes,
    ops::ListQuery,
    pagination::{ListPage, paginate_all},
};
use std::{sync::Arc, time::Duration};

/// Where `cargo make e2e` publishes the engine.
const ENGINE_BASE_URL: &str = "http://127.0.0.1:3100";

/// The prefix the engine serves its API under, matching the deployed configuration.
const ENGINE_API_PREFIX: &str = "/api/catalog";

/// A fictional tenant, per D39.
const ORGANIZATION: &str = "my-org";

/// A fictional tenant, per D39.
const TENANT: &str = "my-tenant";

/// The ACL context the policy layer would have emitted for this tenant.
fn acl_context() -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    URL_SAFE_NO_PAD
        .encode(serde_json::json!({ "organization": ORGANIZATION, "tenant": TENANT }).to_string())
}

/// A client bound to a caller with the full D26 allowlist.
fn client() -> EngineClient {
    client_with(Some(&acl_context()))
}

/// A client bound to a caller carrying `acl` and nothing else.
fn client_with(acl: Option<&str>) -> EngineClient {
    EngineClientFactory::new(
        ENGINE_BASE_URL,
        ENGINE_API_PREFIX,
        Duration::from_secs(5),
        Duration::from_secs(1),
        1,
    )
    .expect("the e2e engine URL is well formed")
    .bind(
        Arc::new(CallerIdentity::new(
            acl,
            Some("3fa85f64-5717-4562-b3fc-2c963f66afa6"),
            None,
            Some("test-request-e2e"),
        )),
        Deadline::starting_now(Duration::from_secs(25)),
    )
}

/// The §13.3 gate: one read operation reaches a live engine and returns a **shaped** result —
/// not a `Value`, but the typed model the tools will read.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_read_reaches_the_live_engine_and_is_shaped() {
    let response = client()
        .list_item_type_definitions(&ListQuery {
            limit: Some(5),
            ..ListQuery::default()
        })
        .await
        .expect("the live engine answers the listing");

    assert!(
        !response.value.items.is_empty(),
        "the engine seeds item type definitions; none came back"
    );

    let itd = &response.value.items[0];

    assert_eq!(itd.api_version, "mia-platform.eu/v1");
    assert_eq!(itd.kind, "ItemTypeDefinition");
    assert!(!itd.spec.names.kind.is_empty());
    assert!(!itd.spec.names.plural.is_empty());
    assert!(
        itd.spec.versions.iter().any(|version| version.served),
        "`{}` has no served version",
        itd.metadata.name
    );
    assert_eq!(
        itd.metadata.name,
        format!("{}.{}", itd.spec.names.plural, itd.spec.group),
        "the engine's own naming rule no longer holds"
    );
}

/// The engine's `continue` token really does walk, and `paginate_all` really does follow it.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_pagination_walks_a_real_listing() {
    let client = client();

    let first = client
        .list_item_type_definitions(&ListQuery {
            limit: Some(2),
            ..ListQuery::default()
        })
        .await
        .expect("the live engine answers the first page");

    assert_eq!(first.value.items.len(), 2);
    let cursor = first
        .value
        .next
        .expect("the seeded catalogue is larger than two types");

    let second = client
        .list_item_type_definitions(&ListQuery {
            limit: Some(2),
            cursor: Some(cursor),
            ..ListQuery::default()
        })
        .await
        .expect("the live engine answers the second page");

    let first_names: Vec<&str> = first
        .value
        .items
        .iter()
        .map(|itd| itd.metadata.name.as_str())
        .collect();

    for itd in &second.value.items {
        assert!(
            !first_names.contains(&itd.metadata.name.as_str()),
            "`{}` came back on both pages",
            itd.metadata.name
        );
    }

    let all = paginate_all(|cursor| {
        let client = client.clone();
        async move {
            let response = client
                .list_item_type_definitions(&ListQuery {
                    limit: Some(50),
                    cursor,
                    ..ListQuery::default()
                })
                .await?;

            Ok::<ListPage<_>, catalog_client::ToolError>(response.value)
        }
    })
    .await
    .expect("the whole listing walks");

    assert!(all.len() > 2, "the full walk returned {} types", all.len());
}

/// `field=spec.names.kind=<kind>` is the point lookup the coordinate resolver rests on (§8.6).
/// Proving it against a live engine is the whole reason that helper can avoid a cache.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_kind_point_lookup_returns_exactly_one_type() {
    let all = client()
        .list_item_type_definitions(&ListQuery {
            limit: Some(200),
            ..ListQuery::default()
        })
        .await
        .expect("the live engine answers the listing");

    let sample = all
        .value
        .items
        .first()
        .expect("the engine seeds item type definitions");
    let kind = sample.spec.names.kind.clone();

    let found = client()
        .list_item_type_definitions(&ListQuery {
            limit: Some(1),
            field: vec![format!("spec.names.kind={kind}")],
            ..ListQuery::default()
        })
        .await
        .expect("the point lookup answers");

    assert_eq!(found.value.items.len(), 1);
    assert_eq!(found.value.items[0].spec.names.kind, kind);
}

/// An empty match is **empty, not unavailable** — the distinction T1 insists on, asserted
/// against the component that actually makes it.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_an_empty_result_is_not_an_error() {
    let response = client()
        .list_item_type_definitions(&ListQuery {
            field: vec!["spec.names.kind=NoSuchKindExists".to_string()],
            ..ListQuery::default()
        })
        .await
        .expect("an empty match is not an error");

    assert!(response.value.items.is_empty());
    assert_eq!(response.value.next, None);
    assert!(response.warnings.is_empty());
}

/// A global item listing is shaped into the typed model, `spec` and all.
///
/// `metadata.family` is asserted **present** on every item: the engine derives it from the
/// item's type on every read (since `v0.9.0`), and `null` means only that the type no longer
/// exists — D30's `unaddressable_item`. §8.1's "address an item from the manifest in hand" rests on
/// it. It is also an optional field in our model, so a rename would otherwise deserialise to
/// `None` silently; this is the assertion that would notice.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_global_item_listing_is_shaped() {
    let response = client()
        .list_items(&ListQuery {
            limit: Some(3),
            ..ListQuery::default()
        })
        .await
        .expect("the live engine answers the listing");

    for item in &response.value.items {
        assert!(item.api_version.contains('/'), "{}", item.api_version);
        assert!(!item.kind.is_empty());
        assert!(!item.metadata.name.is_empty());
        assert!(item.spec.is_object());
        assert!(
            item.metadata
                .family
                .as_deref()
                .is_some_and(|family| !family.is_empty()),
            "`{}` has no family, though its type exists",
            item.metadata.name
        );
    }
}

/// The metadata-only projection really does drop `spec` and keep the full metadata — which is
/// what makes it worth asking for, and what the `Accept` enum has to keep offering.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_partial_projection_is_honoured_by_the_live_engine() {
    let response = client()
        .list_items_partial(&ListQuery {
            limit: Some(3),
            ..ListQuery::default()
        })
        .await
        .expect("the live engine answers the partial listing");

    for item in &response.value.items {
        assert!(!item.metadata.name.is_empty());
        assert!(!item.kind.is_empty());
    }
}

/// The exact warning `catalog-engine` attaches when a `PUT` carries `customFields`
/// (`src/apis/items/upsert/mod.rs`), present since engine `v0.4.0`.
const CUSTOM_FIELDS_IGNORED: &str = "The 'customFields' field cannot be set or updated through \
     this endpoint and will be ignored. To set or update 'customFields', use the dedicated \
     endpoints for managing custom fields.";

/// **§12.3, §15 — the `Warning: 299` path, against the live engine.** The one behaviour no OAS
/// describes: the engine declares the header nowhere, so only a real response can prove that
/// the parser reads it and that the runtime's per-call record keeps it for the model (D28).
///
/// A `PUT` carrying `customFields` is the deterministic trigger — the engine ignores the field
/// and says so. The write is a **raw** `put_item` on purpose: the write cycle's
/// `strip_server_owned` removes `customFields` before sending, precisely so that a tool never
/// reports a write that did not happen, which would leave nothing here to warn about.
///
/// The manifest copies a seeded item's `spec` under a new name, so it is valid against its type
/// by construction and the test depends on no fixture of its own. The environment is torn down
/// with its volume after every run, so the fixed name cannot collide with a previous one.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_warning_299_reaches_the_client_and_the_calls_record() {
    let template = ItemAddress::new("ai.mia-platform.eu", "v1", "agents", "catalog-agent")
        .expect("a well-formed address");
    let probe = ItemAddress::new("ai.mia-platform.eu", "v1", "agents", "e2e-warning-probe")
        .expect("a well-formed address");

    let seeded = client()
        .get_item(&template)
        .await
        .expect("the engine seeds `catalog-agent`")
        .value;

    let manifest = serde_json::json!({
        "apiVersion": seeded.api_version,
        "kind": seeded.kind,
        "metadata": { "name": probe.name() },
        "spec": seeded.spec,
        "customFields": { "e2e-probe": "ignored" },
    });

    let writer = client();
    let response = writer
        .put_item(&probe, &manifest, false)
        .await
        .expect("the engine accepts the item and ignores its `customFields`");

    assert_eq!(response.value.metadata.name, probe.name());

    let warning = response
        .warnings
        .iter()
        .find(|warning| warning.text == CUSTOM_FIELDS_IGNORED)
        .unwrap_or_else(|| {
            panic!(
                "the `customFields` warning did not arrive; got {:?}",
                response.warnings
            )
        });
    assert_eq!(warning.code, 299);

    let collected = writer
        .call_warnings()
        .collected()
        .expect("the call reached the engine");
    assert!(
        collected
            .iter()
            .any(|warning| warning.text == CUSTOM_FIELDS_IGNORED),
        "the call's record lost the warning the response carried: {collected:?}"
    );
}

/// A read of something that is not there is `not_found`, with the remedy the model can act on.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_missing_item_maps_to_not_found() {
    let address = ItemAddress::new("ai.mia-platform.eu", "v1", "agents", "no-such-item")
        .expect("a well-formed address");

    let error = client()
        .get_item(&address)
        .await
        .expect_err("a missing item is an error");

    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(error.remedy, catalog_client::Remedy::RetryAfterChange);
}

/// **D47, end to end against the component that owns the rule.**
///
/// With no ACL context the engine answers `400 "Missing required header x-mia-acl-context"`.
/// The point is not that it fails — it is that the failure comes from **there** and arrives as
/// an ordinary tool error. This server issued no `401` of its own, added no default tenant, and
/// refused nothing: it forwarded what arrived and let the owner decide.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_missing_acl_context_is_the_engines_decision_not_ours() {
    let error = client_with(None)
        .list_items(&ListQuery::default())
        .await
        .expect_err("the engine requires an ACL context");

    assert_eq!(error.code, codes::INVALID_INPUT);
    assert!(
        error.message.to_lowercase().contains("x-mia-acl-context"),
        "the engine's own message should reach the model: {}",
        error.message
    );
}

/// The identity pair really does reach the engine: with a tenant that has no items, a read
/// succeeds and is scoped — which only happens because the header arrived.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_acl_context_is_accepted_verbatim_by_the_engine() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    // A context carrying a `tenantName`, which is the field a re-encoding would drop.
    let with_name = URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "organization": ORGANIZATION,
            "tenant": TENANT,
            "tenantName": "My Tenant",
        })
        .to_string(),
    );

    let response = client_with(Some(&with_name))
        .list_item_type_definitions(&ListQuery {
            limit: Some(1),
            ..ListQuery::default()
        })
        .await
        .expect("the engine accepts the context we forwarded");

    assert_eq!(response.value.items.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// T11 `list_tenants` — the worked example, against the live engine (§13.5).
//
// **What this environment can and cannot prove, stated plainly.** There is no gateway here and
// no authz service, deliberately: the compose file exists to prove *our* forwarding, not the
// policy's regeneration. So the `502` row below is verified live and the `401` row is not —
// producing a live `401` needs authz configured *with* token exchange and no `Authorization`
// header, which is a cluster, not a compose file. The `401` path is asserted against the mock in
// `src/tools/list_tenants/tests.rs`, and the remaining half of §13.5's gate is a dev-cluster
// check this repository cannot run.
// ---------------------------------------------------------------------------------------------

/// **T11-D4, live.** With authz unconfigured the engine answers `502`, and the tool must say
/// *authz* rather than *the catalog* — every other tool may be working perfectly, and a model
/// told "the catalog is unavailable" would stop doing things it could still do.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_tenant_listing_reports_authz_rather_than_the_catalog() {
    let error = client()
        .list_tenants()
        .await
        .expect_err("authz is not configured in this environment");

    assert_eq!(error.code, codes::UPSTREAM_UNAVAILABLE);
    assert_eq!(error.remedy, catalog_client::Remedy::Retry);
    assert!(
        error.message.contains("authorization service"),
        "the message must name authz: {}",
        error.message
    );
    assert!(
        !error.message.to_lowercase().contains("the catalog is"),
        "the message must not blame the catalog: {}",
        error.message
    );
}

/// The endpoint really is reached at `/bff/tenants` and takes no parameters, asserted against
/// the engine itself rather than a document describing it.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_tenant_listing_is_reachable_where_the_client_expects_it() {
    // A `502` is the authz service's absence, not a routing failure: a wrong path would be a
    // `404`, which would map to `not_found` instead.
    let error = client()
        .list_tenants()
        .await
        .expect_err("authz is not configured in this environment");

    assert_ne!(
        error.code,
        codes::NOT_FOUND,
        "`/bff/tenants` is not where this client looks for it"
    );
}
