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
use crate::registry::{
    ToolDescriptor,
    contract::{CallContext, Tool, ToolOutput},
};
use catalog_client::{
    EngineCursor, Remedy, ToolCursor, ToolError,
    error::codes,
    models::{
        MeContext, OwnerRef, Principal, PrincipalType, is_valid_email, normalise_principal_id,
        principal::{ME_KIND_SERVICE_ACCOUNT, ME_KIND_USER},
    },
    ops::principals::PrincipalQuery,
    pagination::{MAX_LIMIT, fingerprint},
};
use rmcp::model::ToolAnnotations;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The tool name, as the model calls it.
pub const TOOL_NAME: &str = "list_principals";

/// What the tool does, and what its rows are for.
///
/// The name says *principals*, not *owners*, so that an agent asked who owns an item does not
/// reach for it: the description is where the purpose is, and `apply_item`'s `owner` text points
/// back here.
const TOOL_DESCRIPTION: &str = "Finds who can own an item: the tenant's users and service \
     accounts, or with `me: true` you alone. Each row's `owner` is the value for apply_item's \
     `metadata.owner`; `owner.ref` is the id search_catalog filters `metadata.owner` by. Filters \
     combine; pass `cursor` to continue.";

/// At most this many rows per answer: the website's owner picker shows as many. The agent is
/// choosing an owner, not exporting the directory.
pub const MAX_ROWS: usize = 20;

/// At most this many `ids` per call. The engine takes 200; twenty covers *"who owns these items"*
/// and keeps the argument bounded.
pub const MAX_IDS: usize = 20;

/// The longest `displayName` or `email` filter accepted, in bytes.
pub const MAX_FILTER_BYTES: usize = 200;

/// How many engine pages one call reads when it filters rows itself, before it answers with what
/// it found and a cursor.
pub const MAX_SCANNED_PAGES: usize = 5;

/// The argument names, as errors report them.
const ME_FIELD: &str = "me";
const TYPE_FIELD: &str = "type";
const IDS_FIELD: &str = "ids";
const DISPLAY_NAME_FIELD: &str = "displayName";
const EMAIL_FIELD: &str = "email";
const CURSOR_FIELD: &str = "cursor";

/// The engine's message for a request that arrived without a tenant context.
const ACL_CONTEXT_HEADER_NAME: &str = "x-mia-acl-context";

/// Arguments for `list_principals`.
#[derive(Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct ListPrincipalsInput {
    /// true: only you, the caller. Takes no other argument.
    #[serde(rename = "me")]
    pub me: Option<bool>,

    /// Only users or only service accounts.
    #[serde(rename = "type")]
    pub principal_type: Option<PrincipalTypeArgument>,

    /// Only these ids (an owner's `ref`), 1–20.
    #[serde(rename = "ids")]
    pub ids: Option<Vec<String>>,

    /// Part of the display name, any case.
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,

    /// Part of the e-mail, any case; users only.
    #[serde(rename = "email")]
    pub email: Option<String>,

    /// The previous answer's `cursor`, to continue.
    #[serde(rename = "cursor")]
    pub cursor: Option<String>,
}

/// A principal's type, as the tool takes it.
#[derive(Clone, Copy, Deserialize, schemars::JsonSchema)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub enum PrincipalTypeArgument {
    // No doc comments on the variants: schemars would turn the enum into a `oneOf` of described
    // constants, paid on every `tools/list`.
    #[serde(rename = "user")]
    User,

    #[serde(rename = "serviceAccount")]
    ServiceAccount,
}

impl From<PrincipalTypeArgument> for PrincipalType {
    fn from(argument: PrincipalTypeArgument) -> Self {
        match argument {
            PrincipalTypeArgument::User => Self::User,
            PrincipalTypeArgument::ServiceAccount => Self::ServiceAccount,
        }
    }
}

/// One row: who someone is, and the reference that makes them an item's owner.
///
/// Absent fields are **omitted, never null**. The id is not repeated beside `owner`: `owner.ref`
/// is the id.
#[derive(Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct PrincipalRow {
    #[serde(rename = "owner")]
    owner: OwnerRef,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    principal_type: Option<PrincipalType>,

    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,

    #[serde(rename = "email", skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

/// The whole answer.
#[derive(Serialize)]
struct ListPrincipalsOutput {
    #[serde(rename = "principals")]
    principals: Vec<PrincipalRow>,

    /// Where the next answer starts; omitted on the last one, and always with `me`.
    #[serde(rename = "cursor", skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,

    /// Only when a whole e-mail address matched nobody: how to give the item to that address.
    #[serde(rename = "hint", skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// What a cursor pins besides the engine's own: how many rows of the engine page it starts on
/// were already returned.
#[derive(Serialize, Deserialize)]
struct PrincipalsPinned {
    #[serde(rename = "s")]
    skip: usize,
}

/// The filters of a listing, checked and normalised.
struct Listing {
    principal_type: Option<PrincipalType>,
    ids: Option<Vec<String>>,
    display_name: Option<String>,
    email: Option<String>,
}

impl Listing {
    /// Whether rows are matched here, besides what the engine applied: only `search` narrows by
    /// name or e-mail, and authz matches it over fields of its own choosing.
    fn filters_locally(&self) -> bool {
        self.display_name.is_some() || self.email.is_some()
    }

    /// The `search` sent to narrow the engine's pages: the e-mail when given, being the more
    /// selective.
    fn search(&self) -> Option<String> {
        self.email.clone().or_else(|| self.display_name.clone())
    }

    /// Whether `principal` matches every filter matched here.
    fn matches(&self, principal: &Principal) -> bool {
        contains_filter(
            principal.display_name.as_deref(),
            self.display_name.as_deref(),
        ) && contains_filter(principal.email.as_deref(), self.email.as_deref())
    }

    /// What a cursor is bound to.
    fn fingerprint(&self) -> String {
        fingerprint(&json!({
            "type": self.principal_type.map(|principal_type| principal_type.as_str()),
            "ids": self.ids,
            "displayName": self.display_name,
            "email": self.email,
        }))
    }
}

/// `true` when there is no `filter`, or `field` contains it under full-Unicode lowercasing.
fn contains_filter(field: Option<&str>, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(filter) => {
            field.is_some_and(|field| field.to_lowercase().contains(&filter.to_lowercase()))
        }
    }
}

/// `list_principals` — who can own an item, as references ready to write.
///
/// **Not a catalog read.** Both routes are proxied by the engine to the authz service, so their
/// failures are about identity and authz, as for `list_tenants`.
pub struct ListPrincipals;

impl Tool for ListPrincipals {
    type Input = ListPrincipalsInput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new::<ListPrincipalsInput>(
            TOOL_NAME,
            TOOL_DESCRIPTION,
            ToolAnnotations::new().read_only(true),
        )
    }

    async fn call(
        &self,
        context: &CallContext,
        input: Self::Input,
    ) -> Result<ToolOutput, ToolError> {
        if input.me == Some(true) {
            refuse_with_me(&input)?;

            return me(context).await;
        }

        let cursor = input.cursor.clone();
        let listing = validate(input)?;

        list(context, &listing, cursor.as_deref()).await
    }
}

/// `me` names one principal, so a filter on it has no meaning: refused rather than ignored, which
/// would hide a misunderstanding.
fn refuse_with_me(input: &ListPrincipalsInput) -> Result<(), ToolError> {
    let other = [
        (TYPE_FIELD, input.principal_type.is_some()),
        (IDS_FIELD, input.ids.is_some()),
        (DISPLAY_NAME_FIELD, input.display_name.is_some()),
        (EMAIL_FIELD, input.email.is_some()),
        (CURSOR_FIELD, input.cursor.is_some()),
    ]
    .into_iter()
    .find_map(|(field, given)| given.then_some(field));

    match other {
        None => Ok(()),
        Some(field) => Err(invalid(
            field,
            format!("`{ME_FIELD}` takes no other argument: remove `{field}`, or `{ME_FIELD}`."),
        )),
    }
}

/// The input bounds, checked before anything reaches the engine.
fn validate(input: ListPrincipalsInput) -> Result<Listing, ToolError> {
    let ids = input.ids.map(validate_ids).transpose()?;
    let display_name = input
        .display_name
        .map(|value| validate_filter(DISPLAY_NAME_FIELD, value))
        .transpose()?;
    let email = input
        .email
        .map(|value| validate_filter(EMAIL_FIELD, value))
        .transpose()?;

    let principal_type = match (input.principal_type.map(PrincipalType::from), &email) {
        (Some(PrincipalType::ServiceAccount), Some(_)) => {
            return Err(invalid(
                EMAIL_FIELD,
                "Only users have an e-mail: remove `email`, or ask for `type: \"user\"`."
                    .to_string(),
            ));
        }
        // Only users have an e-mail, so an e-mail filter asks for users.
        (None, Some(_)) => Some(PrincipalType::User),
        (principal_type, _) => principal_type,
    };

    Ok(Listing {
        principal_type,
        ids,
        display_name,
        email,
    })
}

/// Between 1 and [`MAX_IDS`] UUIDs, each in the engine's own spelling.
fn validate_ids(ids: Vec<String>) -> Result<Vec<String>, ToolError> {
    if ids.is_empty() || ids.len() > MAX_IDS {
        return Err(invalid(
            IDS_FIELD,
            format!(
                "`{IDS_FIELD}` takes between 1 and {MAX_IDS} principal ids; omit it to list \
                 everyone."
            ),
        ));
    }

    ids.into_iter()
        .map(|id| {
            normalise_principal_id(&id).ok_or_else(|| {
                invalid(
                    IDS_FIELD,
                    format!("`{id}` is not a principal id: an id is a UUID."),
                )
            })
        })
        .collect()
}

/// A non-blank filter of at most [`MAX_FILTER_BYTES`], trimmed.
fn validate_filter(field: &str, value: String) -> Result<String, ToolError> {
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Err(invalid(
            field,
            format!("`{field}` is empty: omit it to list everyone, or give part of one."),
        ));
    }

    if trimmed.len() > MAX_FILTER_BYTES {
        return Err(invalid(
            field,
            format!(
                "`{field}` is {} bytes long; at most {MAX_FILTER_BYTES} are accepted.",
                trimmed.len()
            ),
        ));
    }

    Ok(trimmed.to_string())
}

/// The caller, as one row.
async fn me(context: &CallContext) -> Result<ToolOutput, ToolError> {
    let response = context.engine().get_me().await.map_err(directory_error)?;

    render(ListPrincipalsOutput {
        principals: vec![me_row(response.value)],
        cursor: None,
        hint: None,
    })
}

/// `/bff/me` onto the row `/bff/principals` would give: its `kind` spelled the listing's way, and
/// the engine's own display-name fallback.
fn me_row(me: MeContext) -> PrincipalRow {
    let principal_type = match me.kind.as_deref() {
        Some(ME_KIND_USER) => Some(PrincipalType::User),
        Some(ME_KIND_SERVICE_ACCOUNT) => Some(PrincipalType::ServiceAccount),
        _ => None,
    };

    let display_name = match principal_type {
        Some(PrincipalType::ServiceAccount) => [&me.client_name, &me.subject]
            .into_iter()
            .find_map(non_blank),
        _ => [&me.name, &me.preferred_username, &me.email, &me.subject]
            .into_iter()
            .find_map(non_blank),
    }
    .or_else(|| non_blank(&Some(me.id.clone())));

    let email = match principal_type {
        Some(PrincipalType::ServiceAccount) => None,
        _ => non_blank(&me.email),
    };

    PrincipalRow {
        owner: OwnerRef::principal(me.id),
        principal_type,
        display_name,
        email,
    }
}

/// `value`, unless absent or only whitespace — as the engine treats a blank name.
fn non_blank(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// One answer of the directory: up to [`MAX_ROWS`] rows, and where the next one starts.
async fn list(
    context: &CallContext,
    listing: &Listing,
    cursor: Option<&str>,
) -> Result<ToolOutput, ToolError> {
    let fingerprint = listing.fingerprint();
    let (mut start, mut skip) = match cursor {
        None => (None, 0),
        Some(raw) => {
            let cursor = ToolCursor::<PrincipalsPinned>::decode(raw, &fingerprint)?;
            (cursor.engine_cursor(), cursor.pinned.skip)
        }
    };

    let limit = if listing.filters_locally() {
        MAX_LIMIT
    } else {
        MAX_ROWS as u32
    };

    let mut rows = Vec::new();
    let mut scanned = 0_usize;
    let mut next: Option<(Option<EngineCursor>, usize)> = None;

    'pages: for _ in 0..MAX_SCANNED_PAGES {
        let page = context
            .engine()
            .list_principals(&PrincipalQuery {
                limit: Some(limit),
                cursor: start.clone(),
                search: listing.search(),
                principal_type: listing.principal_type,
                ids: listing.ids.clone(),
            })
            .await
            .map_err(directory_error)?
            .value;

        let count = page.items.len();
        for (index, principal) in page.items.into_iter().enumerate().skip(skip) {
            scanned += 1;
            if !listing.matches(&principal) {
                continue;
            }

            rows.push(row(principal));
            if rows.len() == MAX_ROWS {
                // Resume on this page when rows are left on it, otherwise on the next one.
                next = if index + 1 < count {
                    Some((start, index + 1))
                } else {
                    page.next.map(|engine| (Some(engine), 0))
                };
                break 'pages;
            }
        }

        skip = 0;
        match page.next {
            Some(engine) => start = Some(engine),
            None => {
                next = None;
                break 'pages;
            }
        }

        // Past the last page read, so a cursor resumes where this answer stopped looking.
        next = Some((start.clone(), 0));

        // Without a local filter every row matches, so one engine page is one answer.
        if !listing.filters_locally() {
            break 'pages;
        }
    }

    let exhausted = next.is_none();
    let capped = !exhausted && rows.len() < MAX_ROWS && listing.filters_locally();

    let cursor = next
        .map(|(engine, skip)| {
            ToolCursor::new(engine.as_ref(), &fingerprint, PrincipalsPinned { skip }).encode()
        })
        .transpose()?;

    let hint = (rows.is_empty() && exhausted)
        .then(|| email_owner_hint(listing.email.as_deref()))
        .flatten();

    let output = render(ListPrincipalsOutput {
        principals: rows,
        cursor,
        hint,
    })?;

    Ok(if capped {
        output.with_warning(format!(
            "Checked {scanned} principals without filling an answer; pass `cursor` to keep \
             looking."
        ))
    } else {
        output
    })
}

/// A listed principal as a row.
fn row(principal: Principal) -> PrincipalRow {
    PrincipalRow {
        owner: OwnerRef::principal(principal.id),
        principal_type: principal.principal_type,
        display_name: principal.display_name,
        email: principal.email,
    }
}

/// When a whole e-mail address matched nobody, the owner that is still possible: the address
/// itself, as the website offers in the same case.
fn email_owner_hint(email: Option<&str>) -> Option<String> {
    email.filter(|email| is_valid_email(email)).map(|email| {
        format!(
            "No principal has this e-mail. An owner can also be an e-mail address: {}.",
            json!({ "type": "email", "ref": email })
        )
    })
}

/// The directory's failures, worded for what they are: identity and authz, never the catalog's
/// contents.
fn directory_error(error: ToolError) -> ToolError {
    match error.code {
        codes::SERVER_DEFECT if error.message.contains(ACL_CONTEXT_HEADER_NAME) => ToolError::new(
            codes::UNAUTHENTICATED,
            Remedy::Escalate,
            "The tenant context did not reach the service, so the principals could not be \
             listed. The deployment's identity forwarding needs attention.",
        ),
        codes::FORBIDDEN => ToolError::new(
            codes::FORBIDDEN,
            Remedy::Escalate,
            "You may not list this tenant's principals.",
        )
        .with_next_step(
            "`me: true` still names you; an owner can also be {\"type\": \"email\", \"ref\": \
             <address>}",
        ),
        codes::NOT_FOUND | codes::CONFLICT | codes::UNSUPPORTED_FOR_TYPE => ToolError::new(
            codes::SERVER_DEFECT,
            Remedy::Escalate,
            format!(
                "The principal directory answered unexpectedly: {}",
                error.message
            ),
        ),
        _ => error,
    }
}

/// An `invalid_input` naming the offending argument.
fn invalid(field: &str, message: String) -> ToolError {
    ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
        .with_details(json!({ "field": field }))
}

/// The answer, as the payload.
fn render(output: ListPrincipalsOutput) -> Result<ToolOutput, ToolError> {
    serde_json::to_value(output)
        .map(ToolOutput::new)
        .map_err(|err| {
            ToolError::new(
                codes::SERVER_DEFECT,
                Remedy::Escalate,
                format!("The principals could not be rendered: {err}"),
            )
        })
}

#[cfg(test)]
mod tests;
