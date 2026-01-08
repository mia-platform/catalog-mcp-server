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
#[allow(clippy::useless_concat)]
const DEFAULT_MODULE_FILTERS: &str = concat!(
    "h2=info,",
    // "hyper::client=info,",
    // "hyper_util::client=info,",
    // "tower::buffer::worker=warn",
);

pub fn try_init(bin_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::env;

    if matches!(env::var("RUST_LOG"), Err(env::VarError::NotPresent)) {
        let log_level = env::var("LOG_LEVEL").or_else(|err| {
            if matches!(err, env::VarError::NotPresent) {
                Ok(::tracing::Level::INFO.to_string())
            } else {
                Err(err)
            }
        })?;

        // this is the only place where RUST_LOG variable is set, since it did not exist before
        // therefore it should be safe to read the variable from a multi-thread environment
        unsafe {
            // PANIC: the current usage of set_var does not panic according to its definition
            // (key is not empty, contain a NUL or `=` char, nor value contains the NUL char)
            env::set_var(
                "RUST_LOG",
                format!("{bin_name}={log_level},{DEFAULT_MODULE_FILTERS},{log_level}"),
            )
        };
    };

    Ok(())
}
