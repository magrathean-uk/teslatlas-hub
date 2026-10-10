// SPDX-License-Identifier: AGPL-3.0-only

//! Discovery (`GET /.well-known/teslatlas-hub`): which `hub-http-v1` capability
//! tokens this Hub advertises. A token is advertised only when its routes are
//! served, so the set follows the two things that decide which routes exist:
//! the protected cursor key (cursor-paged history and pack delivery) and
//! paired-device authentication (self-revoke).

/// What a route group needs from the Hub before its token may be advertised.
#[derive(Clone, Copy)]
enum Requires {
    /// Served by every Hub.
    Nothing,
    /// Served only with the protected cursor key: the cursor-paged history
    /// routes sign their cursors with it, and pack delivery needs its signed
    /// manifests.
    CursorKey,
    /// Served only with paired-device authentication: `POST /v1/device/revoke`
    /// answers 404 on a trusted local router.
    PairedDeviceAuthentication,
}

/// Every token this Hub can advertise, in the order of `capabilities` in the
/// committed `hub-http-v1@1.2.0` `profile.json` (a test pins the order against
/// that file). The contract treats the list as a set; a stable order keeps the
/// document byte-stable. `query.enrichment` is deliberately absent: it needs
/// `attributions`, which this Hub does not emit until address and terrain
/// enrichment ship, so advertising it would break the contract.
const ADVERTISABLE: [(&str, Requires); 8] = [
    ("query.vehicles", Requires::Nothing),
    ("query.current", Requires::Nothing),
    ("query.drives", Requires::CursorKey),
    ("query.charges", Requires::CursorKey),
    ("query.charge.samples", Requires::CursorKey),
    ("query.positions", Requires::CursorKey),
    ("sync.packs", Requires::CursorKey),
    ("auth.revoke", Requires::PairedDeviceAuthentication),
];

/// The capability tokens discovery advertises, given whether the Hub holds its
/// protected cursor key and whether it requires paired-device authentication.
pub(super) fn advertised_capabilities(
    has_cursor_key: bool,
    paired_device_authentication: bool,
) -> Vec<&'static str> {
    ADVERTISABLE
        .iter()
        .filter(|(_, requires)| match requires {
            Requires::Nothing => true,
            Requires::CursorKey => has_cursor_key,
            Requires::PairedDeviceAuthentication => paired_device_authentication,
        })
        .map(|(token, _)| *token)
        .collect()
}
