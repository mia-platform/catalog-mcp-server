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
use serde::{Deserialize, Deserializer, Serialize};
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

/// The two kinds of principal: the only ones that can own an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrincipalType {
    /// A person.
    #[serde(rename = "user")]
    User,

    /// A machine identity.
    #[serde(rename = "serviceAccount")]
    ServiceAccount,
}

impl PrincipalType {
    /// The value `/bff/principals` takes and returns.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::ServiceAccount => "serviceAccount",
        }
    }
}

/// A principal as `GET /bff/principals` lists one.
///
/// Only `id` is always there. The engine omits `type` for a principal kind it does not
/// recognise, and `email` for every service account.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct Principal {
    /// The principal id: the `ref` of an owner that names it.
    #[serde(rename = "id")]
    pub id: String,

    /// Whether it is a user or a service account; `None` when absent or unrecognised.
    #[serde(rename = "type", default, deserialize_with = "lenient_principal_type")]
    pub principal_type: Option<PrincipalType>,

    /// The name to show, as the engine derives it.
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,

    /// The e-mail, for a user.
    #[serde(rename = "email", default)]
    pub email: Option<String>,
}

/// Reads a principal's `type`, turning a value this client does not know into `None` rather than
/// failing the whole page over one row.
fn lenient_principal_type<'de, D>(deserializer: D) -> Result<Option<PrincipalType>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<Value>::deserialize(deserializer)?;

    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

/// The part of `GET /bff/me` that says who the caller is.
///
/// The rest — organizations, tenants, roles, groups and their permissions — answers what the
/// caller may do, not who they are, and is not read.
#[derive(Clone, Deserialize)]
#[cfg_attr(any(test, feature = "testing"), derive(Debug, PartialEq))]
pub struct MeContext {
    /// The principal id, the same one `/bff/principals` lists.
    #[serde(rename = "id")]
    pub id: String,

    /// `user` or `service_account` — snake case here, unlike `/bff/principals`.
    #[serde(rename = "kind", default)]
    pub kind: Option<String>,

    /// The identity provider's subject.
    #[serde(rename = "subject", default)]
    pub subject: Option<String>,

    /// The e-mail, for a user.
    #[serde(rename = "email", default)]
    pub email: Option<String>,

    /// The person's name.
    #[serde(rename = "name", default)]
    pub name: Option<String>,

    /// The login name.
    #[serde(rename = "preferredUsername", default)]
    pub preferred_username: Option<String>,

    /// A service account's name.
    #[serde(rename = "clientName", default)]
    pub client_name: Option<String>,
}

/// `/bff/me`'s `kind` for a service account.
pub const ME_KIND_SERVICE_ACCOUNT: &str = "service_account";

/// `/bff/me`'s `kind` for a user.
pub const ME_KIND_USER: &str = "user";

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
                    format!("`{reference}` is not a principal id. Find one with list_principals.")
                }),
            Self::Email { reference } if is_valid_email(&reference) => {
                Ok(Self::Email { reference })
            }
            Self::Email { reference } => Err(format!("`{reference}` is not an e-mail address.")),
        }
    }
}
