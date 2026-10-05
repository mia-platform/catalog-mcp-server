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
// Argument checks more than one tool makes, kept in one place so they cannot drift apart.

use catalog_client::{Remedy, ToolError, address::is_valid_group, error::codes};
use serde_json::json;

/// The argument name, as tools declare it and errors report it.
const GROUP_ARGUMENT: &str = "group";

/// Checks the optional `group` a kind-taking tool accepts.
///
/// A kind is unique per group, not per tenant, so `group` is how a caller says which type a
/// shared `kind` means. `usable` says whether this call gives `group` anything to narrow: a tool
/// that only types a `kind` by it, called without one, does not — while a tool that finds an item
/// by name can narrow the name to one group's items with `group` alone. The value is also bound to
/// the engine's `spec.group` grammar; either failure is an `invalid_input` the model can correct.
pub(crate) fn validate_group(group: Option<&str>, usable: bool) -> Result<(), ToolError> {
    let Some(group) = group else {
        return Ok(());
    };

    let message = if !usable {
        "`group` only says which type a shared `kind` means; give `kind` too.".to_string()
    } else if !is_valid_group(group) {
        format!("`{group}` is not an API group, such as `mia-platform.eu`.")
    } else {
        return Ok(());
    };

    Err(
        ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
            .with_details(json!({ "field": GROUP_ARGUMENT })),
    )
}
