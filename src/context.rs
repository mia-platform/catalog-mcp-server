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
use crate::registry::Registry;
use configuration::Config;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Whether the process is ready to receive traffic, as `/-/ready` reports it (D43).
///
/// It starts **not ready** and is raised once every startup condition holds: the validated
/// configuration and the prebuilt `tools/list` payload today, plus the engine probe once
/// `health.readinessChecksEngine` has something to probe. Shutdown lowers it *before* the drain
/// begins, so the endpoint stops receiving traffic while in-flight calls finish (D42, D43).
#[derive(Debug, Default)]
pub struct Readiness(AtomicBool);

impl Readiness {
    /// Whether every startup condition holds and shutdown has not begun.
    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Records that every startup condition holds.
    pub fn mark_ready(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Records that the process is draining and must stop receiving traffic.
    pub fn mark_draining(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Everything shared across requests, held behind one `Arc` and cloned into each handler (D4).
///
/// The engine client factory, the metrics handle and the rate-limit buckets land here in later
/// steps. Nothing request-scoped ever does: the handler's lifetime is one instance per session
/// in legacy mode and one per request when stateless, so a field here would be a cross-request
/// leak rather than a cache.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The validated configuration.
    pub config: Arc<Config>,

    /// The tool set and its prebuilt `tools/list` payload, built once at startup.
    pub registry: Arc<Registry>,

    /// Whether the process is ready to receive traffic.
    pub readiness: Arc<Readiness>,
}

impl AppState {
    /// Builds the shared state from an already validated configuration, with the tool set this
    /// server ships.
    ///
    /// The state starts **not ready**: raising it is [`Readiness::mark_ready`]'s job, once the
    /// caller has finished every startup step.
    pub fn new(config: Config) -> Self {
        Self::with_registry(config, Registry::with_shipped_tools())
    }

    /// Builds the shared state over a given registry, which is what lets a test drive the
    /// handler against a tool set of its own without a second code path in production.
    pub fn with_registry(config: Config, registry: Registry) -> Self {
        Self {
            config: Arc::new(config),
            registry: Arc::new(registry),
            readiness: Arc::new(Readiness::default()),
        }
    }
}
