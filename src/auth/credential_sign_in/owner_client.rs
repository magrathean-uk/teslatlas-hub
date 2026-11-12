// SPDX-License-Identifier: AGPL-3.0-only

//! The one definition of Tesla's standard public owner client (DEC-088).
//!
//! The Hub's own login, its code exchange and the refresh of its own pair use
//! these values and no others. They match what the Mac app's sign-in window
//! sends to Tesla's authorisation page (`TeslaOAuthFlow` in
//! `macos/TeslatlasHubApp/TeslatlasHubApp/TeslaAuthWindowController.swift`); a
//! test reads that file and fails if the two ever differ. The values are
//! public: the client has no secret, and the code is bound to the owner's
//! browser session by PKCE (RFC 7636, S256).

/// The public owner client id.
pub const OWNER_CLIENT_ID: &str = "ownerapi";

/// The scope of the owner login and of every refresh of its pair, so a refresh
/// never widens it; `offline_access` keeps a refresh token in each answer.
pub const OWNER_SCOPE: &str = "openid email offline_access";

/// The redirect the Mac app registers for the owner login. The exchange sends
/// this exact value and refuses any other before a connection is opened.
pub const OWNER_REDIRECT_URI: &str = "tesla://auth/callback";

/// The global owner authorisation endpoint the Mac app opens.
pub const OWNER_AUTHORIZATION_ENDPOINT: &str = "https://auth.tesla.com/oauth2/v3/authorize";

/// The global owner token endpoint: the code exchange and the refresh of a
/// pair minted by the global issuer.
pub const OWNER_TOKEN_ENDPOINT: &str = "https://auth.tesla.com/oauth2/v3/token";
