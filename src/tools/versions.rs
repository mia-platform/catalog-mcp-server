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
// The SemVer names an item's versions carry: checked here as the engine checks them, with the
// engine's own crate, corrected when the mistake is unambiguous, and — when a name is taken — the
// nearest one that is free.

use catalog_client::{Remedy, ToolError, error::codes};
use regex::Regex;
use serde_json::json;
use std::sync::LazyLock;

/// The argument a version name arrives in.
pub const VERSION_NAME_FIELD: &str = "versionName";

/// A type's API version — `v1`, `v1beta1`, `v2alpha3` — which a model holding a type's coordinates
/// may pass where a version name is wanted. Never corrected into SemVer: it is a different thing.
static API_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r"^v[0-9]+((alpha|beta)[0-9]+)?$").expect("API_VERSION_RE is a constant pattern")
});

/// `major.minor` with nothing else, which `.0` completes.
static MAJOR_MINOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: a compile-time constant pattern.
    Regex::new(r"^[0-9]+\.[0-9]+$").expect("MAJOR_MINOR_RE is a constant pattern")
});

/// Checks a version name as the engine will: a SemVer 2.0.0 version, nothing else.
pub fn validate_version_name(text: &str) -> Result<(), ToolError> {
    if semver::Version::parse(text).is_ok() {
        return Ok(());
    }

    let message = if API_VERSION_RE.is_match(text) {
        format!(
            "`{text}` looks like a type's API version; `{VERSION_NAME_FIELD}` is a SemVer name such \
             as `1.0.0`."
        )
    } else {
        match corrected(text) {
            Some(fixed) => format!("`{text}` is not a SemVer version; did you mean `{fixed}`?"),
            None => format!(
                "`{text}` is not a SemVer version: give `major.minor.patch`, such as `1.4.0` or \
                 `2.0.0-rc.1`."
            ),
        }
    };

    Err(
        ToolError::new(codes::INVALID_INPUT, Remedy::RetryAfterChange, message)
            .with_details(json!({ "field": VERSION_NAME_FIELD })),
    )
}

/// The unambiguous correction of a near-SemVer name: a leading `v` dropped, a missing patch added.
fn corrected(text: &str) -> Option<String> {
    let bare = text.strip_prefix('v').unwrap_or(text);

    if bare != text && semver::Version::parse(bare).is_ok() {
        return Some(bare.to_string());
    }

    MAJOR_MINOR_RE
        .is_match(bare)
        .then(|| format!("{bare}.0"))
        .filter(|candidate| semver::Version::parse(candidate).is_ok())
}

/// The nearest free name to `taken`, among `existing` — compared as text, as the engine keys them.
///
/// A plain version moves its patch number up; a pre-release whose last part is a number moves that
/// number up; anything else has no obvious neighbour, and gets `None`. Build metadata plays no part
/// in ordering, so the suggestion drops it.
pub fn suggest_free(taken: &str, existing: &[String]) -> Option<String> {
    let version = semver::Version::parse(taken).ok()?;
    let is_free = |candidate: &String| !existing.iter().any(|name| name == candidate);
    let attempts = existing.len() + 1;

    if version.pre.is_empty() {
        return (1..=attempts as u64)
            .map(|step| {
                format!(
                    "{}.{}.{}",
                    version.major,
                    version.minor,
                    version.patch + step
                )
            })
            .find(is_free);
    }

    let pre = version.pre.as_str();
    let (prefix, last) = match pre.rsplit_once('.') {
        Some((prefix, last)) => (Some(prefix), last),
        None => (None, pre),
    };
    let number: u64 = last.parse().ok()?;

    (1..=attempts as u64)
        .map(|step| {
            let next = number + step;
            let pre = match prefix {
                Some(prefix) => format!("{prefix}.{next}"),
                None => next.to_string(),
            };
            format!(
                "{}.{}.{}-{pre}",
                version.major, version.minor, version.patch
            )
        })
        .find(is_free)
}

#[cfg(test)]
mod tests {
    use super::{suggest_free, validate_version_name};
    use rstest::rstest;

    /// SemVer names pass; everything else is refused, with the correction when it is unambiguous
    /// and never an API version turned into a version name.
    #[rstest]
    #[case::plain("1.0.0", None)]
    #[case::pre_release("2.0.0-rc.1", None)]
    #[case::build("1.0.0+build.5", None)]
    #[case::leading_v("v1.4.0", Some("did you mean `1.4.0`"))]
    #[case::two_parts("1.4", Some("did you mean `1.4.0`"))]
    #[case::api_version("v1", Some("API version"))]
    #[case::api_beta("v1beta1", Some("API version"))]
    #[case::word("latest", Some("major.minor.patch"))]
    #[case::four_parts("1.0.0.0", Some("major.minor.patch"))]
    #[case::empty("", Some("major.minor.patch"))]
    fn test_a_version_name(#[case] text: &str, #[case] refusal: Option<&str>) {
        match (validate_version_name(text), refusal) {
            (Ok(()), None) => {}
            (Err(error), Some(expected)) => {
                assert!(error.message.contains(expected), "{}", error.message);
            }
            (outcome, expected) => panic!("`{text}`: {outcome:?}, expected {expected:?}"),
        }
    }

    /// The nearest free name, compared as text.
    #[rstest]
    #[case::next_patch("1.4.0", &["1.4.0"], Some("1.4.1"))]
    #[case::skips_taken("1.4.0", &["1.4.0", "1.4.1"], Some("1.4.2"))]
    #[case::numbered_pre_release("2.0.0-rc.1", &["2.0.0-rc.1", "2.0.0-rc.2"], Some("2.0.0-rc.3"))]
    #[case::bare_numbered_pre_release("2.0.0-1", &["2.0.0-1"], Some("2.0.0-2"))]
    #[case::unnumbered_pre_release("1.0.0-beta", &["1.0.0-beta"], None)]
    #[case::build_dropped("1.4.0+b1", &["1.4.0+b1"], Some("1.4.1"))]
    fn test_the_free_name(
        #[case] taken: &str,
        #[case] existing: &[&str],
        #[case] expected: Option<&str>,
    ) {
        let existing: Vec<String> = existing.iter().map(|name| (*name).to_string()).collect();

        assert_eq!(suggest_free(taken, &existing).as_deref(), expected);
    }
}
