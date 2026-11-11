// SPDX-License-Identifier: AGPL-3.0-only

//! Tests for the window core. `collection_window` covers grants, the issuer,
//! the gate, hard expiry and the supervisor; `window_resume` covers restart,
//! clock rollback, handback and import admission. Row-level import catch-up
//! is proven by M03-T04, not here.

mod collection_window;
mod support;
mod window_resume;
