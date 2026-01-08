/**
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

pub type Shutdown = broadcast::Sender<()>;

static SHUTDOWN: OnceCell<Shutdown> = OnceCell::const_new();

pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    let mut rx = SHUTDOWN
        .get()
        .map(|tx| tx.subscribe())
        .expect("shutdown signal not initialized");
    async move {
        let _ = rx.recv().await;
    }
}

pub fn register_shutdown_listeners() {
    use futures::FutureExt;
    use tokio::signal::ctrl_c;

    let (tx, _) = broadcast::channel(1);

    let shutdown_fut = async move {
        ctrl_c().await.ok();
    };

    {
        let tx = tx.clone();
        tokio::spawn(shutdown_fut.then(|unit| async move {
            let exit_code = tx.send(unit);

            if let Err(err) = exit_code
                && tx.receiver_count() > 0
            {
                panic!("cannot send shutdown signal: {err}");
            }
        }));
    }

    if SHUTDOWN.set(tx).is_err() {
        panic!("shutdown signal already initialized");
    }
}
