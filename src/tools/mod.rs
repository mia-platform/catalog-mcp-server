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
/// The Step 1 probe: proves the handler, the transport and the identity hook without any
/// catalog logic behind them (§13.2), and the smallest worked example of the §5.5 contract.
pub mod hello;

/// T3 — one item, what it is and what it is connected to, in one call. Wave 1.a.
pub mod describe_item;

/// T1 — every item type the caller can see, with the coordinates to address its items. The
/// first tool of wave 1.a.
pub mod list_catalog_types;

/// T2 — search the catalog by free text, type, labels and fields. Wave 1.a.
pub mod search_catalog;

/// The worked example of the contract freeze, and the cheapest end-to-end probe of the identity
/// path in the whole tool set (§13.5, T11).
pub mod list_tenants;
