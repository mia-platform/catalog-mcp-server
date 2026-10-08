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
    models::{Item, ItemTypeDefinition, PartialObjectMetadata},
    testing::{mock_item, mock_item_type_definition},
};
use rstest::rstest;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// A link the engine accepts: its own schema example is a bare `{"url": …}`.
fn mock_titleless_links() -> Value {
    json!([
        { "url": "https://example.com/runbook" },
        { "url": "https://example.com/docs", "title": "Docs" }
    ])
}

/// An item, its partial projection and a type definition, each carrying `links`.
fn mock_with_links(which: &str) -> Value {
    let mut value = match which {
        "item" => mock_item("example-item"),
        "partial" => {
            let mut item = mock_item("example-item");
            item.as_object_mut().map(|object| object.remove("spec"));
            item
        }
        _ => mock_item_type_definition("Service", "services", "stable.example.com"),
    };
    value["metadata"]["links"] = mock_titleless_links();
    value
}

/// Decodes `value` as `T` and renders it back.
fn round_trip<T: DeserializeOwned + Serialize>(value: Value) -> Value {
    let decoded: T = serde_json::from_value(value).expect("a title-less link decodes");
    serde_json::to_value(decoded).expect("renders")
}

/// Every read model sharing `ObjectMetadata` accepts a title-less link, and renders it without a
/// `title` — omitted, never `null`.
#[rstest]
#[case::item("item", round_trip::<Item> as fn(Value) -> Value)]
#[case::partial_object_metadata("partial", round_trip::<PartialObjectMetadata>)]
#[case::item_type_definition("itd", round_trip::<ItemTypeDefinition>)]
fn test_a_titleless_link_decodes_and_renders_without_a_title(
    #[case] which: &str,
    #[case] round_trip: fn(Value) -> Value,
) {
    let rendered = round_trip(mock_with_links(which));

    assert_eq!(rendered["metadata"]["links"], mock_titleless_links());
    assert!(rendered["metadata"]["links"][0].get("title").is_none());
}

// ---------------------------------------------------------------------------------------------
// Owners.
// ---------------------------------------------------------------------------------------------

/// An owner is read as either form the engine stores, a principal id normalised to the engine's
/// own spelling so that what is written compares equal to what is stored.
#[rstest]
#[case::principal(
    json!({ "type": "principal", "ref": "3fa85f64-5717-4562-b3fc-2c963f66afa6" }),
    crate::models::OwnerRef::principal("3fa85f64-5717-4562-b3fc-2c963f66afa6")
)]
#[case::principal_uppercase(
    json!({ "type": "principal", "ref": "3FA85F64-5717-4562-B3FC-2C963F66AFA6" }),
    crate::models::OwnerRef::principal("3fa85f64-5717-4562-b3fc-2c963f66afa6")
)]
#[case::email(
    json!({ "type": "email", "ref": "ada@example.com" }),
    crate::models::OwnerRef::Email { reference: "ada@example.com".to_string() }
)]
fn test_an_owner_is_parsed(#[case] value: Value, #[case] expected: crate::models::OwnerRef) {
    assert_eq!(crate::models::OwnerRef::parse(&value), Ok(expected));
}

/// Anything else is refused with a sentence, never passed on for the engine to reject.
#[rstest]
#[case::not_a_uuid(json!({ "type": "principal", "ref": "ada" }), "not a principal id")]
#[case::malformed_email(json!({ "type": "email", "ref": "ada@localhost" }), "not an e-mail")]
#[case::unknown_type(json!({ "type": "group", "ref": "devs" }), "must be")]
#[case::resolved_principal(
    json!({ "type": "principal", "ref": "3fa85f64-5717-4562-b3fc-2c963f66afa6",
            "principal": { "displayName": "Ada" } }),
    "no other field"
)]
#[case::missing_ref(json!({ "type": "email" }), "must be")]
#[case::a_string(json!("ada@example.com"), "must be")]
fn test_a_wrong_owner_is_refused(#[case] value: Value, #[case] expected: &str) {
    let message = crate::models::OwnerRef::parse(&value).expect_err("refused");

    assert!(message.contains(expected), "{message}");
}

/// An owner renders as exactly `{type, ref}`.
#[rstest]
fn test_an_owner_renders_as_type_and_ref() {
    assert_eq!(
        serde_json::to_value(crate::models::OwnerRef::principal("p-1")).expect("serialisable"),
        json!({ "type": "principal", "ref": "p-1" })
    );
}
