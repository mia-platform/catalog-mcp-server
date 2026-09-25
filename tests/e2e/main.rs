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
    CallerIdentity, Deadline, EngineClient, EngineClientFactory, FamilyAddress, FieldPath,
    ItemAddress, Predicate, RegexLiteral, coordinates_of,
    error::codes,
    find_item_type, find_item_type_document,
    models::{ItdListEntry, ItemTypeDefinition, RelationshipDirection},
    ops::{ListQuery, relationships::RelationshipQuery},
    pagination::{ListPage, MAX_LIMIT, paginate_all},
    select_served_version,
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
                .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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

/// How many item types the pinned engine seeds (`assets/manifests/type-definitions/`, 68 in
/// `0.9.2`). A floor, not an exact count, so a release that adds one does not fail the run.
const SEEDED_TYPE_COUNT: usize = 68;

/// **T1 against the live engine** — what its contract test used to assert against a vendored
/// OAS, asserted against the engine itself (T1 §11, decision (C)).
///
/// The lean `ItdListEntry` model reads every seeded type, walked to exhaustion as T1 walks it,
/// and every one has the four coordinates T1 returns and a version the core's rule can select.
/// `history.enabled` is optional in the model, so a rename would read as `false` everywhere
/// without failing; asserting that **some** seeded type has it on is what would notice.
/// `llmDescription` cannot be pinned this way — no seeded type carries one.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_type_listing_reads_every_seeded_type_through_the_lean_model() {
    let client = client();

    let entries: Vec<ItdListEntry> = paginate_all(|cursor| {
        let client = &client;
        async move {
            client
                .list_item_type_definitions::<ItdListEntry>(&ListQuery {
                    limit: Some(MAX_LIMIT),
                    cursor,
                    ..ListQuery::default()
                })
                .await
                .map(|response| response.value)
        }
    })
    .await
    .expect("the live engine answers the type listing");

    assert!(
        entries.len() >= SEEDED_TYPE_COUNT,
        "expected at least {SEEDED_TYPE_COUNT} seeded types, got {}",
        entries.len()
    );

    for entry in &entries {
        let spec = &entry.spec;
        assert!(!spec.names.kind.is_empty(), "a type has no kind");
        assert!(
            !spec.names.plural.is_empty(),
            "`{}` has no family",
            spec.names.kind
        );
        assert!(!spec.group.is_empty(), "`{}` has no group", spec.names.kind);
        assert!(
            select_served_version(&spec.versions).is_some(),
            "`{}` has no version the core's rule can select",
            spec.names.kind
        );
    }

    assert!(
        entries.iter().any(|entry| entry
            .spec
            .history
            .as_ref()
            .is_some_and(|history| history.enabled)),
        "no seeded type reports history enabled — has `spec.history.enabled` been renamed?"
    );
}

/// The seeded family the cursor walk pages through: 12 `tools.ai.mia-platform.eu` items, written
/// by no other test, so the walk cannot race a write.
const WALKED_FAMILY: (&str, &str, &str) = ("ai.mia-platform.eu", "v1", "tools");

/// Small enough that the walked family spans at least three pages (T2 §10, §12).
const WALK_PAGE_SIZE: u32 = 5;

/// **T2 against the live engine: a cursor walk returns every item exactly once.** Driven through
/// the same operations `search_catalog` uses — the family listing with the metadata-only
/// projection, then its count — so the engine's `continue` semantics are proven on the path the
/// tool takes, and the count agrees with what the walk saw.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_family_walk_returns_every_item_exactly_once() {
    let client = client();
    let (group, version, family) = WALKED_FAMILY;
    let family = FamilyAddress::new(group, version, family).expect("a well-formed family");

    let mut names = Vec::new();
    let mut pages = 0;
    let mut cursor = None;
    loop {
        let page = client
            .list_family_items_partial(
                &family,
                &ListQuery {
                    limit: Some(WALK_PAGE_SIZE),
                    cursor,
                    ..ListQuery::default()
                },
            )
            .await
            .expect("the live engine answers the page")
            .value;
        pages += 1;
        names.extend(page.items.into_iter().map(|item| item.metadata.name));

        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    let distinct: std::collections::BTreeSet<&String> = names.iter().collect();
    assert!(pages >= 3, "the walk spanned only {pages} pages");
    assert_eq!(
        distinct.len(),
        names.len(),
        "an item was returned twice: {names:?}"
    );

    let count = client
        .count_family_items(&family, &ListQuery::default())
        .await
        .expect("the live engine answers the count")
        .value
        .count;
    assert_eq!(
        count,
        names.len() as u64,
        "the count disagrees with the walk"
    );
}

/// **T2-P2's regression guard: `matches` on `metadata.tags` hits any element of the array.**
/// No seeded item carries tags, so the test writes one — `catalog-agent`'s `spec` under a new
/// name, with two tags — and searches for the second.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_matches_on_tags_hits_one_element_of_the_array() {
    let client = client();
    let template = ItemAddress::new("ai.mia-platform.eu", "v1", "agents", "catalog-agent")
        .expect("a well-formed address");
    let probe = ItemAddress::new("ai.mia-platform.eu", "v1", "agents", "e2e-tags-probe")
        .expect("a well-formed address");
    let seeded = client
        .get_item(&template)
        .await
        .expect("the engine seeds `catalog-agent`")
        .value;
    client
        .put_item(
            &probe,
            &serde_json::json!({
                "apiVersion": seeded.api_version,
                "kind": seeded.kind,
                "metadata": { "name": probe.name(), "tags": ["alpha-tag", "beta-tag"] },
                "spec": seeded.spec,
            }),
            false,
        )
        .await
        .expect("the engine accepts the tagged item");

    let agents = FamilyAddress::new("ai.mia-platform.eu", "v1", "agents").expect("a family");
    let found = |text: &'static str| {
        let client = &client;
        let agents = &agents;
        async move {
            let rawq = Predicate::Matches {
                field: FieldPath::new("metadata.tags").expect("a filterable field"),
                pattern: RegexLiteral::containing(text).expect("a valid literal"),
            }
            .encode_rawq()
            .expect("it encodes");

            client
                .list_family_items_partial(
                    agents,
                    &ListQuery {
                        raw_query: rawq,
                        ..ListQuery::default()
                    },
                )
                .await
                .expect("the live engine answers the search")
                .value
                .items
                .into_iter()
                .any(|item| item.metadata.name == "e2e-tags-probe")
        }
    };

    assert!(
        found("BETA").await,
        "a pattern matching the second tag finds the item"
    );
    assert!(!found("gamma").await, "a pattern matching no tag does not");
}

/// The seeded agent T3's e2e test describes, and the agent its relationship points at.
const DESCRIBED_AGENT: &str = "catalog-agent";
const RELATED_AGENT: &str = "assisted-ai-resource-generator";

/// The seeded relationship type the test links them with.
const DEPENDENCY_TYPE: &str =
    "urn:mia-platform-catalog:mia-platform.eu:v1:RelationshipType:dependency.mia-platform.eu";

/// An agent's URN, as the engine builds it.
fn agent_urn(name: &str) -> String {
    format!("urn:mia-platform-catalog:ai.mia-platform.eu:v1:Agent:{name}")
}

/// Writes a `dependency` relationship from the described agent to `target`.
async fn relate(client: &EngineClient, name: &str, target: &str) {
    let address = ItemAddress::new("mia-platform.eu", "v1", "relationships", name)
        .expect("a well-formed relationship address");

    client
        .put_item(
            &address,
            &serde_json::json!({
                "apiVersion": "mia-platform.eu/v1",
                "kind": "Relationship",
                "metadata": { "name": name },
                "spec": {
                    "sourceRef": agent_urn(DESCRIBED_AGENT),
                    "targetRef": agent_urn(target),
                    "typeRef": DEPENDENCY_TYPE,
                },
            }),
            false,
        )
        .await
        .expect("the engine accepts the relationship");
}

/// **T3 against the live engine** — what its contract and integration lines asked of the
/// relationships endpoint, asserted on the endpoint itself (T3 §8, decision (C)).
///
/// A `groupBy`-free request answers with a flat `List` whose entries parse as
/// `{direction, relationship, relatedItem}`, with `relationship` the **full** record — `typeRef`,
/// `sourceRef`, `targetRef` all present under the metadata-only projection, which T3-D1's
/// client-side grouping rests on. And with no `acl-filter` on this path, an entry whose other end
/// does not exist comes back **without** `relatedItem` rather than being dropped — the case T3-D7
/// reports as `unresolved`.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_the_relationships_listing_is_flat_and_keeps_unresolved_entries() {
    let client = client();
    relate(&client, "e2e-rel-resolved", RELATED_AGENT).await;
    relate(&client, "e2e-rel-dangling", "no-such-agent").await;

    let described = ItemAddress::new("ai.mia-platform.eu", "v1", "agents", DESCRIBED_AGENT)
        .expect("a well-formed address");
    let entries = client
        .get_relationships(
            &described,
            &RelationshipQuery {
                direction: Some(RelationshipDirection::Outbound),
                ..RelationshipQuery::default()
            },
        )
        .await
        .expect("the live engine answers with a flat list")
        .value
        .items;

    let resolved = entries
        .iter()
        .find(|entry| entry.relationship.metadata.name == "e2e-rel-resolved")
        .expect("the resolved relationship is listed");
    assert_eq!(resolved.direction, RelationshipDirection::Outbound);
    assert_eq!(resolved.type_ref(), Some(DEPENDENCY_TYPE));
    assert_eq!(
        resolved.other_end(),
        Some(agent_urn(RELATED_AGENT).as_str())
    );
    assert_eq!(
        resolved
            .related_item
            .as_ref()
            .map(|item| item.metadata.name.as_str()),
        Some(RELATED_AGENT)
    );

    let dangling = entries
        .iter()
        .find(|entry| entry.relationship.metadata.name == "e2e-rel-dangling")
        .expect("an entry whose other end does not exist is still listed");
    assert!(
        dangling.related_item.is_none(),
        "its relatedItem is omitted, not invented"
    );
    assert_eq!(
        dangling.other_end(),
        Some(agent_urn("no-such-agent").as_str())
    );
}

/// The seeded types T6's definition of done names — the smallest useful one, the depth-table
/// example and the largest shipped type.
const T6_NAMED_KINDS: [&str; 3] = ["Skill", "AgenticWorkflow", "Campaign"];

/// **T6 against the live engine, over every seeded type** — what the plan asked 68 vendored
/// fixtures to prove, proven on the engine itself instead, so nothing is copied and nothing can go
/// stale (T6 §7, §9, decision (C); DR-82).
///
/// Each type goes through T6's exact path: the `(group, kind)` lookup (two rows asked for, exactly
/// one back), and the core's version selection, which must land on a version carrying a schema.
/// And the document it answers with must carry the type's `spec` **byte for byte as the engine's
/// own listing does** — every version, every field, the ones this client's model does not declare
/// included — because T6 hands that `spec` back untouched and T12 edits from it (DR-86).
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_every_seeded_type_returns_its_definition_whole() {
    let client = client();
    let listed = client
        .list_all_item_type_definitions::<serde_json::Value>()
        .await
        .expect("the live engine lists its types");
    assert!(
        listed.len() >= SEEDED_TYPE_COUNT,
        "only {} types listed",
        listed.len()
    );

    for expected in &listed {
        let (kind, group) = (
            expected["spec"]["names"]["kind"].as_str().expect("a kind"),
            expected["spec"]["group"].as_str().expect("a group"),
        );
        // The exact `(group, kind)` lookup: a kind is unique per group only (DR-80).
        let (found, _) = find_item_type_document(&client, kind, Some(group))
            .await
            .unwrap_or_else(|err| panic!("`{kind}` does not resolve: {err:?}"));
        let coordinates = coordinates_of(&found.definition, kind).expect("a served version");

        assert!(
            found
                .definition
                .spec
                .versions
                .iter()
                .find(|version| version.name == coordinates.version)
                .and_then(|version| version.schema.as_ref())
                .and_then(|schema| schema.get("openAPIV31Schema"))
                .is_some(),
            "`{kind}` has no schema where T6 reads it"
        );
        assert_eq!(
            found.raw["spec"], expected["spec"],
            "`{kind}`'s definition is not whole"
        );
    }

    for kind in T6_NAMED_KINDS {
        assert!(
            listed
                .iter()
                .any(|definition| definition["spec"]["names"]["kind"] == kind),
            "`{kind}` is not seeded"
        );
    }
}

/// **DR-80, live: a kind is unique per group, not per tenant.** `Service` is seeded in two groups,
/// so a lookup by `kind` alone is answered with both as candidates — the premise the plans had
/// wrong, pinned against the engine that disproved it.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_shared_kind_is_answered_with_its_groups() {
    let error = find_item_type(&client(), "Service", None)
        .await
        .expect_err("a shared kind is never resolved without a group");

    assert_eq!(error.code, codes::NOT_FOUND);
    let groups: Vec<String> = error
        .details
        .as_deref()
        .and_then(|details| details["candidates"].as_array().cloned())
        .expect("candidates")
        .iter()
        .map(|candidate| candidate["group"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        groups.len() >= 2
            && groups
                .iter()
                .any(|group| group == "console.mia-platform.eu"),
        "{groups:?}"
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
///
/// It arrives as `server_defect` / `Escalate`: the request carried nothing of the caller's, so
/// the model is not told to change arguments it cannot change (§8.4). On the in-cluster path a
/// missing context *is* a deployment defect — headers not forwarded — which is what an operator
/// should read, and the engine's own reason is carried so they can.
#[tokio::test]
#[ignore = "needs `cargo make e2e`"]
async fn test_a_missing_acl_context_is_the_engines_decision_not_ours() {
    let error = client_with(None)
        .list_items(&ListQuery::default())
        .await
        .expect_err("the engine requires an ACL context");

    assert_eq!(error.code, codes::SERVER_DEFECT);
    assert_eq!(error.remedy, catalog_client::Remedy::Escalate);
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
        .list_item_type_definitions::<ItemTypeDefinition>(&ListQuery {
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
