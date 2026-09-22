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
use crate::registry::{PER_TOOL_ALLOWANCE, Registry};
use rstest::{fixture, rstest};
use serde_json::Value;

/// Words that must never appear anywhere in a tool's advertised surface (D21, NFR-01).
///
/// The ACL context, the bearer token and the principal id exist only in the request-scoped
/// identity, which a tool receives as an opaque capability it cannot address. With hand-written
/// tools there is no code path from a tool argument to an outbound header — this test is what
/// stops one being added by accident.
const FORBIDDEN_SCHEMA_TERMS: &[&str] = &[
    "acl",
    "authorization",
    "bearer",
    "header",
    "organization",
    "principal",
    "tenant",
    "token",
    "x-mia",
];

/// Where a tool's minified input schema is recorded, byte for byte (D17).
const GOLDEN_DIR: &str = "src/schema/golden";

#[fixture]
fn mock_registry() -> Registry {
    Registry::with_shipped_tools()
}

/// The specification asks for a deterministic order, and `list_all()` sorts by name — so
/// nothing sorts twice, and this asserts the property rather than the implementation.
#[rstest]
fn test_tool_order_is_deterministic(mock_registry: Registry) {
    let names: Vec<&str> = mock_registry
        .tools()
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect();

    let mut sorted = names.clone();
    sorted.sort_unstable();

    assert_eq!(names, sorted);
}

/// D22 — one registry, one payload, and `get_tool` answers for everything in it.
#[rstest]
fn test_every_listed_tool_is_retrievable(mock_registry: Registry) {
    for tool in mock_registry.tools() {
        assert!(
            mock_registry.tool(tool.name.as_ref()).is_some(),
            "`{}` is listed but not retrievable",
            tool.name
        );
    }
}

/// D17 — `get_tool` feeds the SDK's `Mcp-Param-*` validation, so it must serve **byte-for-byte**
/// what `list_tools` served. Two sources for one schema would diverge silently.
#[rstest]
fn test_get_tool_schema_is_byte_identical_to_the_listed_one(mock_registry: Registry) {
    for listed in mock_registry.tools() {
        let retrieved = mock_registry
            .tool(listed.name.as_ref())
            .expect("a listed tool is retrievable");

        assert_eq!(
            serde_json::to_vec(&listed.input_schema).expect("a serialisable schema"),
            serde_json::to_vec(&retrieved.input_schema).expect("a serialisable schema"),
            "`{}` serves two different schemas",
            listed.name
        );
    }
}

/// D15 — the measured headline defect of the previous server was 37 519 B of identical
/// `outputSchema`: 33 % of the payload, zero information. This gets a test, not a convention.
#[rstest]
fn test_no_tool_advertises_an_output_schema(mock_registry: Registry) {
    for tool in mock_registry.tools() {
        let serialised: Value =
            serde_json::to_value(tool).expect("a tool descriptor is serialisable");

        assert!(
            serialised.get("outputSchema").is_none(),
            "`{}` advertises an outputSchema",
            tool.name
        );
    }
}

/// D21 — asserted by walking every registered schema, so a header parameter cannot be
/// reintroduced without failing CI. This is the ACL hole the rewrite exists to close.
#[rstest]
fn test_no_schema_mentions_an_identity_or_a_header(mock_registry: Registry) {
    for tool in mock_registry.tools() {
        let schema = serde_json::to_string(&tool.input_schema)
            .expect("a serialisable schema")
            .to_lowercase();

        for term in FORBIDDEN_SCHEMA_TERMS {
            assert!(
                !schema.contains(term),
                "`{}`'s input schema mentions `{term}`: {schema}",
                tool.name
            );
        }
    }
}

/// D17 — every emitted schema is asserted byte-for-byte against a golden file, so a change to
/// the minifier or to an input type shows up as a reviewable diff.
///
/// Regenerate deliberately with `UPDATE_GOLDEN=1 cargo test`.
#[rstest]
fn test_minified_schemas_match_their_goldens(mock_registry: Registry) {
    for tool in mock_registry.tools() {
        let path = std::path::Path::new(GOLDEN_DIR).join(format!("{}.json", tool.name));
        let actual = format!(
            "{}\n",
            serde_json::to_string_pretty(&tool.input_schema).expect("a serialisable schema")
        );

        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::create_dir_all(GOLDEN_DIR).expect("the golden directory is writable");
            std::fs::write(&path, &actual).expect("the golden file is writable");
            continue;
        }

        let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "no golden for `{}` at {}: {err}. Run with UPDATE_GOLDEN=1 to record it.",
                tool.name,
                path.display()
            )
        });

        assert_eq!(actual, expected, "`{}`'s schema drifted", tool.name);
    }
}

/// D12, §9 — the one hard limit, on a payload we author. It scales with the tool count so that
/// functionality growing raises the budget automatically and can never create pressure to cut a
/// tool; only bloat — more bytes for the same tools — fails.
#[rstest]
fn test_tools_list_payload_is_inside_its_budget(mock_registry: Registry) {
    let bytes = mock_registry.serialised_bytes();
    let budget = mock_registry.byte_budget();

    // Printed whether the check passes or fails, so growth is always attributable and the review
    // question is "T4 grew 312 bytes, is it earning that?" rather than "the payload is too big".
    println!(
        "tools/list: {bytes} B of {budget} B ({} tools x {PER_TOOL_ALLOWANCE} B)",
        mock_registry.tools().len()
    );
    for (name, tool_bytes) in mock_registry.byte_table() {
        println!("  {name:<32} {tool_bytes:>6} B");
    }

    assert!(
        bytes <= budget,
        "the tools/list payload is {bytes} B, over its {budget} B ceiling"
    );
}

#[rstest]
fn test_byte_budget_scales_with_the_tool_count(mock_registry: Registry) {
    assert_eq!(
        mock_registry.byte_budget(),
        PER_TOOL_ALLOWANCE * mock_registry.tools().len()
    );
}
