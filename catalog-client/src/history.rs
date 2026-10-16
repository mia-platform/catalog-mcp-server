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
// Whether an item's type keeps history, and how to say so when it does not: the two refusals a
// history write meets — a kind that can never record, and a type whose recording is off — and the
// engine's own wording of them, read only when the refusal arrives unannounced.

use crate::error::{Remedy, ToolError, codes};
use serde_json::json;

/// The group of the four kinds that can never have history.
const CORE_GROUP: &str = "mia-platform.eu";

/// The families whose items are not stored as ordinary items, and so can never have history.
pub const NO_HISTORY_FAMILIES: [&str; 4] = [
    "relationships",
    "relationship-types",
    "relationship-constraints",
    "custom-fields",
];

/// The engine's key phrase for a kind that can never record.
const STRUCTURAL_PHRASE: &str = "cannot have revision history";

/// The engine's key phrase for a type whose recording is off. The frozen-history warning shares it,
/// so it is only ever matched on an error.
const NOT_RECORDED_PHRASE: &str = "is not being recorded";

/// Why a history write cannot be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryRefusal {
    /// The kind's items are not stored as items: nothing can ever change this.
    Unsupported,

    /// The type's recording is off: someone can switch it on.
    NotRecorded,
}

impl HistoryRefusal {
    /// The refusal a type is certain to meet, known before any call: `group`/`family` from its
    /// coordinates, `history_enabled` from its definition.
    pub fn of_type(group: &str, family: &str, history_enabled: bool) -> Option<Self> {
        if group == CORE_GROUP && NO_HISTORY_FAMILIES.contains(&family) {
            Some(Self::Unsupported)
        } else if !history_enabled {
            Some(Self::NotRecorded)
        } else {
            None
        }
    }

    /// The refusal an engine `422` stands for, read from its message — only for one that arrives
    /// although the type looked able to record (its setting changed in between). `None` for a `422`
    /// of another kind.
    pub fn of_engine_message(message: &str) -> Option<Self> {
        if message.contains(STRUCTURAL_PHRASE) {
            Some(Self::Unsupported)
        } else if message.contains(NOT_RECORDED_PHRASE) {
            Some(Self::NotRecorded)
        } else {
            None
        }
    }

    /// Whether nothing can ever change it.
    pub fn is_permanent(self) -> bool {
        matches!(self, Self::Unsupported)
    }

    /// The error the model reads, naming `kind`.
    pub fn to_error(self, kind: &str) -> ToolError {
        let message = match self {
            Self::Unsupported => format!(
                "Items of kind `{kind}` can never have history: they are not stored as ordinary items."
            ),
            Self::NotRecorded => format!(
                "History is not recorded for `{kind}`. Someone with permission to change the type \
                 definition can switch it on (in the website, the type's history settings), and every \
                 existing item then gets a revision; no tool here switches it on."
            ),
        };

        ToolError::new(codes::UNSUPPORTED_FOR_TYPE, Remedy::Escalate, message)
            .with_details(json!({ "kind": kind, "permanent": self.is_permanent() }))
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryRefusal;
    use rstest::rstest;

    /// The four kinds never record; any other type follows its setting.
    #[rstest]
    #[case::relationships(
        "mia-platform.eu",
        "relationships",
        true,
        Some(HistoryRefusal::Unsupported)
    )]
    #[case::custom_fields(
        "mia-platform.eu",
        "custom-fields",
        true,
        Some(HistoryRefusal::Unsupported)
    )]
    #[case::same_family_elsewhere("example.com", "relationships", true, None)]
    #[case::recording_off("example.com", "services", false, Some(HistoryRefusal::NotRecorded))]
    #[case::recording_on("example.com", "services", true, None)]
    fn test_a_types_refusal(
        #[case] group: &str,
        #[case] family: &str,
        #[case] enabled: bool,
        #[case] expected: Option<HistoryRefusal>,
    ) {
        assert_eq!(HistoryRefusal::of_type(group, family, enabled), expected);
    }

    /// The engine's two messages, verbatim in shape, and one of another kind.
    #[rstest]
    #[case::structural(
        "Items of family 'relationships' in API group 'mia-platform.eu/v1' cannot have revision history",
        Some(HistoryRefusal::Unsupported)
    )]
    #[case::not_recorded(
        "Revision history is not being recorded for items of family 'services' in API group \
         'example.com/v1': set 'spec.history.enabled' on its Item Type Definition to resume",
        Some(HistoryRefusal::NotRecorded)
    )]
    #[case::other("Custom fields are not supported for items of family 'views'", None)]
    fn test_an_engine_message(#[case] message: &str, #[case] expected: Option<HistoryRefusal>) {
        assert_eq!(HistoryRefusal::of_engine_message(message), expected);
    }

    /// The setting's refusal names the setting; the permanent one does not.
    #[rstest]
    fn test_the_two_wordings() {
        let unsupported = HistoryRefusal::Unsupported.to_error("Relationship");
        let not_recorded = HistoryRefusal::NotRecorded.to_error("Service");

        assert!(!unsupported.message.contains("switch it on"));
        assert!(not_recorded.message.contains("switch it on"));
        assert_eq!(
            unsupported
                .details
                .as_deref()
                .map(|d| d["permanent"].clone()),
            Some(true.into())
        );
        assert_eq!(
            not_recorded
                .details
                .as_deref()
                .map(|d| d["permanent"].clone()),
            Some(false.into())
        );
    }
}
