// SPDX-License-Identifier: AGPL-3.0-only

//! Supplied-token input: there is none (M02-T02, M02-T10).
//!
//! Source credentials are disabled. The Hub stores a sign-in only from its own
//! login, so no access or refresh token handed to it (typed, piped, read from a
//! file, decrypted from another application, or a provider's) is parsed or
//! kept. The refusal-only entry points T02 left for the old `setup` and
//! `setup-fleet` token inputs are gone with those inputs (ruling R6): the
//! command tree no longer has a token option, `setup` forwards only the sign-in
//! code material to the running service, and Fleet setup is refused
//! `provider_parked`. No flag, setting, variable or acknowledgement adds one.
