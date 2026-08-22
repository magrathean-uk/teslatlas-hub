# Goal: working Rust TeslaMate replacement

## Result

Ship `hub/` as a practical one-vehicle TeslaMate replacement for Apple-silicon
macOS and Debian ARM64. New Mac users connect their Tesla account in the native
app, configure Hub without token files, install the embedded service package,
and start collection. Linux keeps full CLI/package operation.

## Normal development

Work sequentially on `hub/main`:

1. Read `PROGRESS.md` and select the next missing product behavior.
2. Reproduce it or define one exact hermetic check.
3. Make the smallest correct source change.
4. Run one focused check.
5. Record the result in `PROGRESS.md` and commit.

`PROGRESS.md` is the only work ledger. Do not create Fleet branches, copied
repositories, evidence trees, or separate target directories.

## Product order

1. Keep the HTTP trust boundary honest: plaintext stays loopback-only and is
   not described as authenticated.
2. Keep exactly one refresh-token owner: the resident collector. Do not expose
   wake/climate commands or construct command-only credential managers.
3. Keep normal backup data-only. Exclude decryption/signing keys. Offer key
   disaster recovery only as an explicit, separately encrypted export.
4. Integrate Tesla Auth-compatible PKCE login in the macOS app using native
   WebKit. Pass tokens to the embedded Rust CLI through bounded stdin, never
   files, argv, UI output, or logs. Configure before package install so the
   package can start a ready service.
5. Preserve direct read-only TeslaMate v4.1.1 migration, compact disk use,
   macOS LaunchAgent operation, Debian systemd packaging, local sync, repair,
   and one-vehicle telemetry collection.
6. Run final Rust/macOS checks once. Run Linux package/runtime proof only when
   Linux behavior or dependencies changed; reuse the one normal Cargo cache.

## Resource limits

- Work only in `hub/`; never edit or build `app/`.
- Use this checkout and `hub/target` only.
- Never create clones, worktrees, copied source trees, extra Cargo target
  directories, evidence archives, benchmark data, or long-lived VM images.
- Run one Cargo process at a time. Use focused tests while coding and one final
  suite/check/Clippy/release pass near handoff.
- If a disposable Linux guest is required, keep one ARM64 guest below 6 GiB
  under `/tmp` and delete it immediately after verification.
- Clean disposable debug/test artefacts before handoff; retain at most the
  normal release cache needed for the next developer.

## Safety

- TeslaMate PostgreSQL access remains read-only.
- Never print or persist plaintext Tesla tokens outside the protected Hub
  credential store. Never place them in process arguments.
- Never send vehicle wake, climate, or other control commands.
- Never run two processes that can refresh the same token pair.
- Never expose the plaintext loopback listener through forwarding, proxying,
  tunnelling, or a non-loopback bind without TLS and authentication.
- Never push, publish, deploy, or change remote systems without explicit user
  direction. Preserve unrelated user files.

## Done

Done means the native macOS login/setup/install path works hermetically; default
backup excludes keys; encrypted credential recovery round-trips and rejects
wrong keys/tampering/overwrite; one refresh owner is enforced; Rust format,
tests, Clippy, and release build pass; the AppKit app builds/tests; Linux still
cross-checks or package-tests as appropriate; temporary build/test data is
removed; and `PROGRESS.md` separates completed proof from deferred physical
driving, live refresh-expiry, clean-host install, iOS sync, and endurance tests.
