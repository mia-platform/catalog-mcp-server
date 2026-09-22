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
use std::future::Future;
use tokio::sync::{OnceCell, broadcast};

/// The broadcast half every shutdown listener subscribes to.
pub type Shutdown = broadcast::Sender<()>;

static SHUTDOWN: OnceCell<Shutdown> = OnceCell::const_new();

/// Resolves when the process has been asked to stop.
///
/// # Panics
///
/// Panics when [`register_shutdown_listeners`] has not run yet, which is a programming error
/// rather than a runtime condition.
pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    let mut rx = SHUTDOWN
        .get()
        .map(|tx| tx.subscribe())
        .expect("shutdown signal not initialized");

    async move {
        let _ = rx.recv().await;
    }
}

/// Spawns the task that turns `SIGTERM` and ctrl-c into one broadcast (D42).
///
/// `SIGTERM` is the one Kubernetes sends; handling only ctrl-c, as the previous server did,
/// means every rollout kills in-flight requests.
///
/// # Panics
///
/// Panics when called twice, or when `SIGTERM` cannot be subscribed to — both are startup
/// programming errors.
pub fn register_shutdown_listeners() {
    use tokio::signal::ctrl_c;

    let (tx, _) = broadcast::channel(1);

    #[cfg(unix)]
    let mut unix_terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("subscribe to SIGTERM");

    {
        let tx = tx.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                tokio::select! {
                    _ = ctrl_c() => {},
                    _ = unix_terminate.recv() => {},
                };
            }

            #[cfg(not(unix))]
            {
                ctrl_c().await.ok();
            }

            let sent = tx.send(());

            if let Err(err) = sent
                && tx.receiver_count() > 0
            {
                panic!("cannot send shutdown signal: {err}");
            }
        });
    }

    if SHUTDOWN.set(tx).is_err() {
        panic!("shutdown signal already initialized");
    }
}
