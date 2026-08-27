# Design QA — onboarding copy and TeslaMate server import

Status: **PASS** (2026-08-27)

## Evidence

- Source visual truth:
  - `/Users/bolyki/.codex/attachments/8a7fee6c-f89d-464a-8b0d-9da2826407a0/image-1.png`
  - `/var/folders/8k/z8sw10td7vb127frh96hg3900000gn/T/TemporaryItems/NSIRD_screencaptureui_WbOkB6/Screenshot 2026-08-27 at 18.02.53.png`
  - `/var/folders/8k/z8sw10td7vb127frh96hg3900000gn/T/TemporaryItems/NSIRD_screencaptureui_pi7iuK/Screenshot 2026-08-27 at 18.07.14.png`
- Implementation and combined comparison screenshots were generated under
  `target/design-qa`, reviewed at native size, then removed with the other
  generated build output.
- Viewport: native 900×630-point AppKit window plus titlebar, light appearance, 2× capture density.
- Source pixels: welcome crop 550×280; migration crop 1590×646.
- Implementation pixels: welcome 1800×1342; migration key/password 1800×1324 each.
- Normalization: focused implementation regions were cropped at native 2× density; source regions were height-normalized before side-by-side comparison. No browser or CSS scaling applies to this native app.
- States: welcome step 1; TeslaMate import step 3 with key/agent; TeslaMate import step 3 with password.

## Fidelity surfaces

- Typography: native San Francisco weights remain legible and consistent. Intro and sudo guidance wrap cleanly; labels, fields and buttons do not truncate.
- Spacing and rhythm: the four welcome benefits fit without crowding. Import fields use one aligned form grid; the primary server action is large and centered. The inactive duplicate footer action is hidden until import is available.
- Colours: system semantic colours remain native. Primary server action is solid system blue with white text; disabled grey is not used for the connection action.
- Image and icon quality: supplied Hub ICNS stays sharp and continuously rounded. The welcome page uses the actual Rust language logo; remaining native SF Symbols match the existing AppKit design and scale.
- Copy: removed private/local-storage claims. Added the requested Rust, Docker, native macOS/Debian, open-source and SQLite benefits. Import guidance now describes normal-user Docker access and passwordless sudo.
- Interaction: key picker, SSH agent/default-key mode, password mode, normal-user default, passwordless-sudo checkbox and connection action are functional. Password is passed through a protected temporary SSH askpass file, never a process argument, and cleared from the field after connection.
- Keyboard: a standard Edit menu restores Cmd+A/C/X/V/Z. An explicit import key-view loop moves Tab through server, user, port, authentication, credential, sudo and action controls. Back and Continue have no arrows.
- Diagnostics: Cmd+L opens the combined app/import and service-log window. Structured app events are persisted in a 1 MiB bounded, mode-0600 log; SSH discovery, tunnel attempts/readiness, compatibility and cleanup are recorded without server, user or credential values. Copy and Save run the existing share redactor.
- Live status: the dashboard refreshes every five seconds while visible, so the start, first observation and stop states no longer remain stale.

## Comparison history

- Iteration 1 found P1 import lockout: only implicit SSH keys were supported and the default account was root. Added key selection, password authentication, normal-user default and direct-Docker/passwordless-sudo choice.
- Iteration 1 found P2 action hierarchy: Connect to Server rendered as low-emphasis grey text. Replaced it with a large blue primary action.
- Iteration 2 found P2 alignment and duplicate-action noise: the primary action remained left-aligned and a disabled Continue button competed in the footer. Centered Connect to Server and hid Continue until compatibility succeeds.
- Post-fix evidence is in the two combined comparisons and both import authentication screenshots above.
- Live read-only key-auth import-path check reached the TeslaMate database tunnel in 108 ms. Cmd+A replaced the whole server field, Tab moved to the SSH-user field, and Cmd+L displayed every SSH stage. Compatibility was deliberately not completed in the preview-only test app.

## Findings

- No actionable P0, P1 or P2 differences remain.
- P3: SSH password authentication has automated UI/build coverage but still needs a real password-authenticated Debian account for live end-to-end proof.

## Implementation checklist

- Requested welcome copy and benefit list applied.
- Privacy footer removed.
- Footer button widths follow their current labels.
- Key, agent/default-key and password SSH authentication added.
- Normal-user/passwordless-sudo Docker path added.
- Centered primary connection action added.
- Native package rebuilt.
- Rust Foundation logo attribution added to `THIRD_PARTY_NOTICES.md`.
- 83/83 AppKit tests passed; macOS packaging source checks passed.
- Installed UI proof passed for Cmd+L, full diagnostics, live observation, automatic running/stopped refresh and overinstall.
- Final local package: `dist/TeslatlasHub.pkg`, 66,598,668 bytes,
  SHA-256 `b9643a8baca60b2accb73b4c40c6740707f283de0fbe1eeea9337aa91a45f631`.

final result: passed
