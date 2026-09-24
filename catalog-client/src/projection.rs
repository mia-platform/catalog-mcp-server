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
/// `Accept` value asking for the full object.
const ACCEPT_FULL: &str = "application/json";

/// `Accept` value asking for the metadata-only projection.
const ACCEPT_PARTIAL: &str = "application/json;as=PartialObjectMetadata";

/// How much of an object the engine should send back (§8.2).
///
/// `PartialObjectMetadata` is `{apiVersion, kind, metadata, resourceVersion}` with the **full**
/// `ObjectMetadata` inside it — family, labels, tags and title are all present — and no `spec`.
/// Anything outside this enum is a `406` from the engine, which is why it is an enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Projection {
    /// The whole object, `spec` included.
    #[default]
    Full,

    /// Metadata only: no `spec`.
    PartialObjectMetadata,
}

impl Projection {
    /// The `Accept` header value this projection asks for.
    pub fn accept(&self) -> &'static str {
        match self {
            Self::Full => ACCEPT_FULL,
            Self::PartialObjectMetadata => ACCEPT_PARTIAL,
        }
    }
}

/// How the relationships endpoint groups its answer (§8.2).
///
/// The engine rejects `groupBy=type` together with the partial projection, because grouping
/// needs `spec.typeRef` and the partial projection drops it. **The pair is unconstructible here
/// rather than relayed as a `400`**: an invalid combination the model cannot see is worse than
/// one it cannot express.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    /// No grouping. Either projection is legal.
    #[default]
    None,

    /// Group by direction. Answers `kind: "RelationshipList"` with a `groups` array.
    Direction(Projection),

    /// Group by type. Forces the full projection, because that is the only legal pairing.
    Type,
}

impl Grouping {
    /// The `groupBy` query value, when there is one.
    pub fn group_by(&self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Direction(_) => Some("direction"),
            Self::Type => Some("type"),
        }
    }

    /// The projection this grouping implies. `Type` is always [`Projection::Full`].
    pub fn projection(&self) -> Projection {
        match self {
            Self::None => Projection::Full,
            Self::Direction(projection) => *projection,
            Self::Type => Projection::Full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(Projection::Full, "application/json")]
    #[case(
        Projection::PartialObjectMetadata,
        "application/json;as=PartialObjectMetadata"
    )]
    fn test_accept_values_are_the_engines_own(
        #[case] projection: Projection,
        #[case] expected: &str,
    ) {
        assert_eq!(projection.accept(), expected);
    }

    /// The whole point of [`Grouping`]: `groupBy=type` can only ever carry the full projection,
    /// so the `400` the engine would answer is unreachable.
    #[rstest]
    fn test_group_by_type_cannot_carry_the_partial_projection() {
        assert_eq!(Grouping::Type.projection(), Projection::Full);
        assert_eq!(Grouping::Type.group_by(), Some("type"));
    }

    #[rstest]
    fn test_group_by_direction_keeps_its_projection() {
        let grouping = Grouping::Direction(Projection::PartialObjectMetadata);

        assert_eq!(grouping.group_by(), Some("direction"));
        assert_eq!(grouping.projection(), Projection::PartialObjectMetadata);
    }

    #[rstest]
    fn test_no_grouping_sends_no_group_by() {
        assert_eq!(Grouping::None.group_by(), None);
    }
}
