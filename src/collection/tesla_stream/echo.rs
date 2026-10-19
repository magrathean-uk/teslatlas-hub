// SPDX-License-Identifier: AGPL-3.0-only

//! Detection of a credential that the server reflects in a field the Hub keeps
//! or logs (R24, R52).
//!
//! The grammar of a stream field cannot tell a credential from data: a short
//! bearer is printable ASCII within the 32 bytes that `shift_state` allows, and
//! within the grammar that lets an `error_type` be logged. A server (or a
//! provider in front of one) could therefore echo the subscribed token into
//! such a field, and it would be emitted as telemetry, stored in the observation
//! payload or written to the log. The guard compares by digest, so the session
//! keeps no copy of the token after the subscribe frame is written (R6 step 6),
//! only its length and a SHA-256 of it.
//!
//! Two comparisons use that digest. A logged `error_type` is suppressed when it
//! equals the token, at every grammar-sized length of 1 to 32 bytes. The gear
//! and substring comparison (the whole token inside a longer text) keeps a
//! four-byte minimum, because a shorter comparison could remove a genuine gear
//! letter; a reflected token shorter than that is an accepted limit of it.

use sha2::{Digest, Sha256};

/// The longest text a reflecting field can carry: `shift_state` is at most 32
/// bytes (R24) and a loggable `error_type` is at most 32 bytes (R52). A token
/// longer than this cannot be reflected whole into either field, so the guard
/// has nothing to compare for it.
pub(super) const REFLECTABLE_MAX: usize = 32;

/// The shortest token the gear and substring comparison uses. A shorter text is
/// a gear letter or another ordinary value far more likely than a credential,
/// so [`EchoGuard::reflects`] ignores a shorter token. Whole-value equality for
/// logging ([`EchoGuard::loggable`]) is not subject to this minimum.
const REFLECTABLE_MIN: usize = 4;

/// The digest and length of the token a session subscribed with.
#[derive(Clone, Default)]
pub(super) struct EchoGuard {
    /// `None` when there is nothing to compare (no token, or a length no
    /// reflecting field can hold). The length is 1 to [`REFLECTABLE_MAX`].
    target: Option<(usize, [u8; 32])>,
}

impl EchoGuard {
    /// A guard for `token`.
    pub(super) fn of(token: &str) -> Self {
        let length = token.len();
        if !(1..=REFLECTABLE_MAX).contains(&length) {
            return Self::default();
        }
        Self {
            target: Some((length, Sha256::digest(token.as_bytes()).into())),
        }
    }

    /// `true` when `text` is exactly the token, of any retained length.
    pub(super) fn equals(&self, text: &str) -> bool {
        let Some((length, digest)) = self.target else {
            return false;
        };
        text.len() == length && Sha256::digest(text.as_bytes()).as_slice() == digest
    }

    /// `true` when `text` is the token or contains it, for a token of at least
    /// [`REFLECTABLE_MIN`] bytes. Only a text of at most [`REFLECTABLE_MAX`]
    /// bytes is examined; a longer one is not a field a reflected short token
    /// fits into without surrounding text, and the cost stays bounded by one
    /// hash per start position.
    pub(super) fn reflects(&self, text: &str) -> bool {
        let Some((length, digest)) = self.target else {
            return false;
        };
        if length < REFLECTABLE_MIN {
            return false;
        }
        let bytes = text.as_bytes();
        if bytes.len() < length || bytes.len() > REFLECTABLE_MAX {
            return false;
        }
        bytes
            .windows(length)
            .any(|window| Sha256::digest(window).as_slice() == digest)
    }
}

impl EchoGuard {
    /// The `error_type` token as it may be logged (R52): the grammar of
    /// `loggable_error_token`, and `other` for a text that equals the session's
    /// own token at any length, or contains it when the token has four bytes or
    /// more. The grammar alone would let a token of lower-case letters, digits
    /// and underscores through.
    pub(super) fn loggable<'a>(&self, token: Option<&'a str>) -> &'a str {
        match token {
            Some(text) if self.equals(text) || self.reflects(text) => "other",
            _ => super::session::loggable_error_token(token),
        }
    }
}

impl std::fmt::Debug for EchoGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EchoGuard(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reflected_short_token_is_found_whole_or_inside_other_text() {
        let guard = EchoGuard::of("tok-SECRET-1");
        assert!(guard.reflects("tok-SECRET-1"));
        assert!(guard.reflects("x tok-SECRET-1 y"));
        assert!(guard.reflects("Bearer tok-SECRET-1"));
        assert!(!guard.reflects("tok-SECRET-2"));
        assert!(!guard.reflects("D"));
        assert!(!guard.reflects(""));
    }

    #[test]
    fn the_gear_and_substring_guard_ignores_tokens_below_four_bytes() {
        for token in ["", "D", "ab", "abc", &"t".repeat(33), &"t".repeat(8192)] {
            let guard = EchoGuard::of(token);
            assert!(!guard.reflects(token), "{}", token.len());
            assert!(!guard.reflects("D"));
            assert!(!guard.reflects(&format!("x{token}y")), "{}", token.len());
            if token.is_empty() || token.len() > REFLECTABLE_MAX {
                assert!(guard.target.is_none(), "{}", token.len());
            }
        }
        assert!(EchoGuard::default().target.is_none());
        // A token of exactly four bytes is still compared by the guard.
        assert!(EchoGuard::of("abcd").reflects("xabcdx"));
    }

    #[test]
    fn a_short_token_is_never_logged_as_an_error_type() {
        for token in ["x", "ab", "abc", "ai"] {
            let guard = EchoGuard::of(token);
            assert_eq!(guard.loggable(Some(token)), "other", "{token}");
            assert!(guard.equals(token));
            // Same length, different value: retained.
            let different: String = token
                .chars()
                .map(|c| if c == 'z' { 'y' } else { 'z' })
                .collect();
            assert_eq!(different.len(), token.len());
            assert_eq!(guard.loggable(Some(&different)), different);
            // A containing value is not suppressed for a token below four bytes.
            let containing = format!("{token}_ok");
            assert_eq!(guard.loggable(Some(&containing)), containing);
            let containing_before = format!("pre_{token}");
            assert_eq!(guard.loggable(Some(&containing_before)), containing_before);
        }
        // An empty token has nothing to compare: the grammar still hides an
        // empty error type, and no other value is suppressed.
        let empty = EchoGuard::of("");
        assert_eq!(empty.loggable(Some("")), "other");
        assert_eq!(empty.loggable(None), "other");
        assert_eq!(empty.loggable(Some("busy")), "busy");
        // "ai" does not hide an unrelated safe value that merely contains it.
        assert_eq!(EchoGuard::of("ai").loggable(Some("rain")), "rain");
        // A token of four bytes or more also hides a value that contains it.
        assert_eq!(EchoGuard::of("abcd").loggable(Some("x_abcd_y")), "other");
        assert_eq!(EchoGuard::of("abcd").loggable(Some("abce")), "abce");
    }

    #[test]
    fn a_text_longer_than_the_fields_is_not_examined() {
        let guard = EchoGuard::of("tok-SECRET-1");
        let long = format!("{}tok-SECRET-1", "x".repeat(40));
        assert!(!guard.reflects(&long));
    }

    #[test]
    fn the_guard_never_shows_the_token_or_its_digest() {
        let guard = EchoGuard::of("tok-SECRET-1");
        assert_eq!(format!("{guard:?}"), "EchoGuard(..)");
    }
}
