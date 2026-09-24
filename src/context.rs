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
use catalog_client::EngineClientFactory;
use configuration::Config;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
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
/// The metrics handle and the rate-limit buckets land here in a later step. Nothing
/// request-scoped ever does: the handler's lifetime is one instance per session in legacy mode
/// and one per request when stateless, so a field here would be a cross-request leak rather than
/// a cache.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The validated configuration.
    pub config: Arc<Config>,

    /// The tool set and its prebuilt `tools/list` payload, built once at startup.
    pub registry: Arc<Registry>,

    /// One `reqwest` connection pool for the process, bound per request to a caller's identity
    /// (D25). There is no ambient identity here: a request cannot be made from this alone.
    pub engine: Arc<EngineClientFactory>,

    /// Whether the process is ready to receive traffic.
    pub readiness: Arc<Readiness>,
}

impl AppState {
    /// Builds the shared state from an already validated configuration, with the tool set this
    /// server ships.
    ///
    /// The state starts **not ready**: raising it is [`Readiness::mark_ready`]'s job, once the
    /// caller has finished every startup step.
    /// # Errors
    ///
    /// Fails when the configured engine base URL or timeouts cannot produce an HTTP client,
    /// which is a startup condition and not a runtime one.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Self::with_registry(config, Registry::with_shipped_tools())
    }

    /// Builds the shared state over a given registry, which is what lets a test drive the
    /// handler against a tool set of its own without a second code path in production.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn with_registry(config: Config, registry: Registry) -> anyhow::Result<Self> {
        let engine = EngineClientFactory::new(
            &config.engine.base_url,
            &config.engine.api_prefix,
            Duration::from_millis(config.engine.timeout_ms),
            Duration::from_millis(config.engine.connect_timeout_ms),
            config.engine.max_retries,
        )?;

        Ok(Self {
            config: Arc::new(config),
            registry: Arc::new(registry),
            engine: Arc::new(engine),
            readiness: Arc::new(Readiness::default()),
        })
    }

    /// Binds the shared engine client to one caller and one call's deadline (D25).
    ///
    /// The deadline is `tools.callDeadlineSeconds` from now: `engine.timeoutMs` bounds one hop
    /// and this bounds the whole call.
    ///
    // Its production caller is the `CallContext` of §5.5, which Step 4 freezes; until then it is
    // reached only from the tenant-isolation test, which drives the whole identity path through
    // it. Hence the allow, matching `catalog-engine`'s convention for such items.
    #[allow(dead_code)]
    pub fn engine_for(
        &self,
        identity: Arc<catalog_client::CallerIdentity>,
    ) -> catalog_client::EngineClient {
        self.engine.bind(
            identity,
            catalog_client::Deadline::starting_now(Duration::from_secs(
                self.config.tools.call_deadline_seconds,
            )),
        )
    }
}
