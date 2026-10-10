// SPDX-License-Identifier: AGPL-3.0-only

//! The live provider credentials in scope for one provider response, and the
//! value scrub that keeps a reflected credential out of everything the Hub
//! stores, caches or serves.
//!
//! The allowlist in `owner_api` keeps only named scalar fields, but it cannot
//! tell that a kept string is a bearer: a provider, or anything between the
//! Hub and it, that echoes the access or refresh token into an allowlisted
//! field (`display_name`, `vehicle_state.car_version`, ...) would otherwise
//! have it persisted and served to paired devices. Before anything is stored
//! such a string is replaced by JSON null (for a discovered vehicle name,
//! dropped; for a provider vehicle `state` on a list or the state probe,
//! replaced by the Hub's own unknown state), and one warning per response
//! names the JSON paths, never a value.
//!
//! The carrier borrows the tokens from their owners. It has no `Clone` and no
//! derived `Debug`, so a secret cannot be copied out of it or logged through
//! it; its constructors are crate-private so the type can appear in public
//! signatures without letting outside code build one.
//!
//! Limits, stated so nobody expects more: only a whole secret is recognised.
//! A fragment of a long token is not (a substring test of every prefix and
//! suffix would blank unrelated text), and a string over the provider text
//! bound is already dropped by the allowlist before it reaches this scrub.
//! Observations stored before this scrub existed are not rewritten, and Fleet
//! Telemetry delivered through the retired Edge path does not pass through it.

use std::fmt;

use serde_json::{Map, Value};

use super::Vehicle;
use crate::protocol::constant_time_eq;

/// A secret shorter than this is matched only by equality. Matching it inside
/// longer text would blank unrelated words (a 4-byte token is a common word),
/// while real Tesla tokens are far longer than the floor.
const MIN_CONTAINED_SECRET_BYTES: usize = 16;

/// The Hub's own state for a vehicle whose provider-reported state cannot be
/// kept: the value the projection already stores when a payload has no usable
/// `source_vehicle_state`. It keeps the scheduler away from every online,
/// charging and driving branch.
pub(crate) const UNKNOWN_VEHICLE_STATE: &str = "unknown";

/// One live secret in the two forms the scrub compares it in.
struct LiveSecret<'a> {
    /// The credential as given, padding included. An echo that carries the
    /// padding carries these exact bytes, and the 16-byte floor for matching
    /// inside longer text is measured on this length, never on the trimmed one.
    original: &'a str,
    /// The same credential without its ASCII whitespace padding: what a
    /// whole-value comparison uses, because an echo may drop the padding.
    trimmed: &'a str,
}

impl LiveSecret<'_> {
    /// True when `text` contains the secret. Each form is searched for only
    /// when that form is itself at least the floor long: a secret shorter than
    /// the floor is never searched for inside longer text.
    fn is_contained_in(&self, text: &str) -> bool {
        // The verbatim padded bytes, gated on the credential's original length:
        // they cover a 16-byte credential whose trimmed form is shorter.
        let original_found =
            self.original.len() >= MIN_CONTAINED_SECRET_BYTES && text.contains(self.original);
        // The trimmed bytes are an extra check for a long credential echoed
        // without its padding. They carry their own gate on the trimmed length,
        // so a credential that reaches the floor only through its padding never
        // has its shorter trimmed form searched for inside unrelated text.
        let trimmed_found =
            self.trimmed.len() >= MIN_CONTAINED_SECRET_BYTES && text.contains(self.trimmed);
        original_found || trimmed_found
    }
}

/// Borrowed live secrets for one provider response.
pub struct ProviderSecrets<'a> {
    secrets: Vec<LiveSecret<'a>>,
}

impl<'a> ProviderSecrets<'a> {
    /// No live secret is known (Fleet Telemetry ingest): the scrub is a no-op.
    pub(crate) fn none() -> Self {
        Self {
            secrets: Vec::new(),
        }
    }

    /// ASCII whitespace around a secret is not part of it for a whole-value
    /// comparison: credential validation accepts a token that begins or ends
    /// with a space and keeps it, while an echo of that token may arrive with
    /// that space, with other padding or with none, so equality compares the
    /// trimmed secret to the trimmed text. The original bytes are kept next to
    /// it for the substring rule, which depends on the credential's real
    /// length. A secret that is blank after trimming is never stored, so an
    /// empty needle cannot match every blank string.
    pub(crate) fn new(secrets: &[&'a str]) -> Self {
        Self {
            secrets: secrets
                .iter()
                .copied()
                .map(|original| LiveSecret {
                    original,
                    trimmed: original.trim_ascii(),
                })
                .filter(|secret| !secret.trimmed.is_empty())
                .collect(),
        }
    }

    /// True when `text` is, or (for secrets of at least 16 bytes) contains,
    /// a live secret, including a bare `Bearer <secret>` and a secret padded
    /// by ASCII whitespace. Equality ignores the padding of the text and of the
    /// secret; the 16-byte rule for matching inside longer text is measured on
    /// the credential as given (see [`ProviderSecrets::new`]). Exact
    /// comparisons are constant time and every secret is compared, so the
    /// answer does not reveal which one matched.
    pub(crate) fn reflects(&self, text: &str) -> bool {
        if self.secrets.is_empty() {
            return false;
        }
        let trimmed = text.trim_ascii();
        let unprefixed = strip_bearer_scheme(trimmed);
        let mut reflected = false;
        for secret in &self.secrets {
            reflected |= constant_time_eq(trimmed.as_bytes(), secret.trimmed.as_bytes());
            reflected |= constant_time_eq(unprefixed.as_bytes(), secret.trimmed.as_bytes());
            reflected |= secret.is_contained_in(text);
        }
        reflected
    }

    /// Replace every reflected string under `fields` with null and return the
    /// JSON paths that were replaced. Call it only on allowlisted fields: the
    /// paths are then static provider key names, safe to log.
    pub(crate) fn scrub_fields(&self, fields: &mut Map<String, Value>) -> Vec<String> {
        let mut paths = Vec::new();
        if self.secrets.is_empty() {
            return paths;
        }
        for (key, value) in fields.iter_mut() {
            self.scrub_value(value, key, &mut paths);
        }
        paths
    }

    fn scrub_value(&self, value: &mut Value, path: &str, paths: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                if self.reflects(text) {
                    *value = Value::Null;
                    paths.push(path.to_owned());
                }
            }
            Value::Object(fields) => {
                for (key, nested) in fields.iter_mut() {
                    self.scrub_value(nested, &format!("{path}.{key}"), paths);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    self.scrub_value(item, &format!("{path}[{index}]"), paths);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    /// Make a discovered vehicle list safe to store and serve. The display
    /// name of a vehicle that reflects a live secret is dropped (treated as
    /// absent; it would otherwise be stored as the car name) and its provider
    /// `state` is replaced by [`UNKNOWN_VEHICLE_STATE`] (it is stored as the
    /// source vehicle state and served as the current state). One warning per
    /// response, naming the paths and a count only.
    pub(crate) fn scrub_vehicle_list(&self, vehicles: &mut [Vehicle]) {
        let mut vehicles_affected = 0_usize;
        let mut names_dropped = false;
        let mut states_replaced = false;
        for vehicle in vehicles {
            let name_reflected = vehicle
                .display_name
                .as_deref()
                .is_some_and(|name| self.reflects(name));
            if name_reflected {
                vehicle.display_name = None;
                names_dropped = true;
            }
            let state_reflected = self.reflects(&vehicle.state);
            if state_reflected {
                vehicle.state = UNKNOWN_VEHICLE_STATE.to_owned();
                states_replaced = true;
            }
            if name_reflected || state_reflected {
                vehicles_affected += 1;
            }
        }
        if vehicles_affected > 0 {
            let paths = match (names_dropped, states_replaced) {
                (true, true) => "response[].display_name, response[].state",
                (true, false) => "response[].display_name",
                _ => "response[].state",
            };
            tracing::warn!(
                paths,
                vehicles = vehicles_affected,
                "provider vehicle list reflected a live credential; the affected names were dropped and states replaced"
            );
        }
    }

    /// The provider `state` of one vehicle from the state probe: a state that
    /// reflects a live secret becomes [`UNKNOWN_VEHICLE_STATE`]. One warning,
    /// naming the path only.
    pub(crate) fn scrub_vehicle_state(&self, state: String) -> String {
        if !self.reflects(&state) {
            return state;
        }
        tracing::warn!(
            paths = "response.state",
            "provider vehicle state reflected a live credential; it was replaced by the unknown state"
        );
        UNKNOWN_VEHICLE_STATE.to_owned()
    }
}

impl fmt::Debug for ProviderSecrets<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderSecrets([redacted])")
    }
}

/// `Bearer <token>` with the scheme matched case-insensitively (RFC 9110) and
/// any ASCII whitespace after it; anything else is returned unchanged.
fn strip_bearer_scheme(text: &str) -> &str {
    const SCHEME: &str = "bearer";
    match text.get(..SCHEME.len()) {
        Some(head) if head.eq_ignore_ascii_case(SCHEME) => {
            let rest = &text[SCHEME.len()..];
            if rest.starts_with(|c: char| c.is_ascii_whitespace()) {
                rest.trim_ascii_start()
            } else {
                text
            }
        }
        _ => text,
    }
}

/// Scoped capture of `tracing` output for tests that must prove what a
/// warning names and, above all, what it does not.
#[cfg(test)]
pub(crate) mod capture {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    pub(crate) struct LogCapture {
        buffer: Arc<Mutex<Vec<u8>>>,
        _guard: tracing::subscriber::DefaultGuard,
    }

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl LogCapture {
        /// Capture WARN and above on the current thread until dropped. Async
        /// tests must use a current-thread runtime (the `#[tokio::test]`
        /// default) so the code under test runs on this thread.
        pub(crate) fn start() -> Self {
            let buffer = Arc::new(Mutex::new(Vec::new()));
            let writer = Arc::clone(&buffer);
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::WARN)
                .with_writer(move || SharedWriter(Arc::clone(&writer)))
                .finish();
            Self {
                buffer,
                _guard: tracing::subscriber::set_default(subscriber),
            }
        }

        pub(crate) fn contents(&self) -> String {
            String::from_utf8_lossy(&self.buffer.lock().expect("log buffer")).into_owned()
        }
    }
}
