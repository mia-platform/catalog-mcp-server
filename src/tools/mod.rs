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
/// One type's schema, whole, for an agent about to write an item of it.
pub mod get_item_schema;

/// A test-only probe reporting the identity that reached it; never registered in production.
#[cfg(test)]
pub mod echo_identity;

/// Argument checks shared by more than one tool.
mod arguments;

/// Finding one item from its name, and `kind` when given.
mod lookup;

/// `search_catalog`'s filters — free text, type, labels, fields — shared by every tool taking them.
pub(crate) mod filters;

/// What the two compliance tools share: targets, runs, and how a run's result is shaped.
pub(crate) mod compliance;

/// Create or merge-patch one item, through `catalog-client`'s read-merge-write cycle.
pub mod apply_item;

/// Create or merge-patch one type definition, reporting what the engine ignored and what the
/// change means for the items already stored.
pub mod apply_item_type;

/// Set or remove an item's custom fields — the only writer of them — or discover the keys its
/// type can take.
pub mod patch_item_custom_fields;

/// Who did what in the catalog, and when, from its audit log.
pub mod get_audit_log;

/// The SemVer names of item versions: checked, corrected, and the nearest free one.
pub(crate) mod versions;

/// Tag a revision of an item with a SemVer name, so it can be restored later.
pub mod tag_item_version;

/// Replace an item's content with one of its versions, guarded and checked.
pub mod restore_item_version;

/// Delete one type and every item of it, guarded by the item count.
pub mod delete_item_type;

/// Delete one item, reporting what went with it and a cascade that failed.
pub mod delete_item;

/// One item, what it is and what it is connected to, in one call.
pub mod describe_item;

/// Every item type the caller can see, with the coordinates to address its items.
pub mod list_catalog_types;

/// Search the catalog by free text, type, labels and fields.
pub mod search_catalog;

/// Run a compliance rule, scorecard or campaign, wait for it in the cluster, and report what failed.
pub mod run_compliance_evaluation;

/// Read a compliance rule's, scorecard's or campaign's results without running anything.
pub mod get_compliance_results;
