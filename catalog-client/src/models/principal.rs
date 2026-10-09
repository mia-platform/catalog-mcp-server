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
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

/// The engine's e-mail rule for an owner, copied from its `EMAIL_REGEX`: the HTML5 e-mail
/// production with at least one dot in the domain. Holding a value to the same rule here makes the
/// engine's own *"malformed"* `400` unreachable from a value this client accepted.
static EMAIL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(
        r"^[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+(\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+)*@[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)+$",
    )
    .expect("EMAIL_REGEX is a constant pattern")
});

/// Whether `value` is an e-mail address the engine accepts as an owner.
pub fn is_valid_email(value: &str) -> bool {
    EMAIL_REGEX.is_match(value)
}

/// `id` in the engine's own spelling of a principal id (a lowercase, hyphenated UUID), or `None`
/// when it is not one.
pub fn normalise_principal_id(id: &str) -> Option<String> {
    uuid::Uuid::parse_str(id)
        .ok()
        .map(|uuid| uuid.hyphenated().to_string())
}

/// An item's owner, as the engine stores `metadata.owner`.
///
/// Only the two fields a write takes: a resolved `principal` beside them is the engine's to fill
/// on a read and is discarded on a write, so it is refused rather than carried.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq, Eq))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum OwnerRef {
    /// A user or service account of the tenant, by id.
    #[serde(rename = "principal")]
    Principal {
        /// The principal id.
        #[serde(rename = "ref")]
        reference: String,
    },

    /// Someone outside the principal directory, by e-mail.
    #[serde(rename = "email")]
    Email {
        /// The e-mail address.
        #[serde(rename = "ref")]
        reference: String,
    },
}

impl OwnerRef {
    /// The owner reference naming the principal `id`.
    pub fn principal(id: impl Into<String>) -> Self {
        Self::Principal {
            reference: id.into(),
        }
    }

    /// Reads and checks an owner from a tool argument.
    ///
    /// A principal's `ref` must be a UUID, and is returned in the engine's own form (lowercase,
    /// hyphenated) so that what is written compares equal to what is stored. An e-mail must pass
    /// the engine's e-mail rule.
    ///
    /// # Errors
    ///
    /// A sentence for the model saying what is wrong with the value.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let owner: Self = serde_json::from_value(value.clone()).map_err(|_| {
            "`metadata.owner` must be {\"type\": \"principal\", \"ref\": <id>} or \
             {\"type\": \"email\", \"ref\": <address>}, with no other field."
                .to_string()
        })?;

        match owner {
            Self::Principal { reference } => normalise_principal_id(&reference)
                .map(Self::principal)
                .ok_or_else(|| {
                    format!("`{reference}` is not a principal id: a principal's `ref` is a UUID.")
                }),
            Self::Email { reference } if is_valid_email(&reference) => {
                Ok(Self::Email { reference })
            }
            Self::Email { reference } => Err(format!("`{reference}` is not an e-mail address.")),
        }
    }
}
