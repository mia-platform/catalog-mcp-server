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
use crate::models::{Item, PartialObjectMetadata};
use serde::Deserialize;

/// Which end of a relationship the described item is (the engine's `RelationshipDirection`).
///
/// - `Outbound`: the item is the relationship's `spec.sourceRef`.
/// - `Inbound`: the item is its `spec.targetRef`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RelationshipDirection {
    /// The item is the target.
    #[serde(rename = "inbound")]
    Inbound,

    /// The item is the source.
    #[serde(rename = "outbound")]
    Outbound,
}

impl RelationshipDirection {
    /// The value the engine takes in its `direction` query parameter and uses on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

/// One entry of the relationships listing (`GET /bff/…/relationships`, no `groupBy`).
///
/// `relationship` is always the **full** relationship item — the engine needs its `sourceRef`,
/// `targetRef` and `typeRef` — and the metadata-only projection applies to `relatedItem` alone.
/// `relatedItem` is **omitted**, not null, when the other end could not be resolved (T3 §0).
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct ItemRelationshipEntry {
    /// Which end the described item is.
    #[serde(rename = "direction")]
    pub direction: RelationshipDirection,

    /// The relationship record itself.
    #[serde(rename = "relationship")]
    pub relationship: Item,

    /// The item on the other end, when the engine resolved it.
    #[serde(rename = "relatedItem", default)]
    pub related_item: Option<PartialObjectMetadata>,
}

impl ItemRelationshipEntry {
    /// The relationship type's URN, `spec.typeRef`.
    pub fn type_ref(&self) -> Option<&str> {
        self.relationship
            .spec
            .get("typeRef")
            .and_then(|value| value.as_str())
    }

    /// The URN of the other end: `targetRef` for an outbound entry, `sourceRef` for an inbound
    /// one — the same classification the engine applies.
    pub fn other_end(&self) -> Option<&str> {
        let reference = match self.direction {
            RelationshipDirection::Outbound => "targetRef",
            RelationshipDirection::Inbound => "sourceRef",
        };

        self.relationship
            .spec
            .get(reference)
            .and_then(|value| value.as_str())
    }
}
