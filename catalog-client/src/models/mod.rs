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
// `Debug` and `PartialEq` on these models are gated on `any(test, feature = "testing")` rather
// than on `test` alone. A `cfg(test)` gate only fires while *this* crate is compiled as a test,
// so a downstream integration test — or a tool's own test in the binary crate — would not get
// them. Release builds still carry neither.

/// Items and their metadata.
pub mod item;

/// Item Type Definitions.
pub mod item_type_definition;

/// The engine's list and count envelopes.
pub mod list;

/// A relationship between two items, as the relationships listing returns it (T3).
pub mod relationship;

/// Tenants, as the authz service describes them.
pub mod tenant;

pub use item::{Item, Link, ObjectMetadata, PartialObjectMetadata};
pub use item_type_definition::{
    ItdHistory, ItdListEntry, ItdSpec, ItdVersion, ItemTypeDefinition, ItemTypeDefinitionSpec,
    TypeNames, TypeVersion,
};
pub use list::{Count, ListEnvelope, ListMetadata};
pub use relationship::{ItemRelationshipEntry, RelationshipDirection};
pub use tenant::Tenant;
