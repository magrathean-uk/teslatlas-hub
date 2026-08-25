# Goal: working Rust TeslaMate replacement

## Result

Ship `hub/` as a practical multi-vehicle TeslaMate replacement for Apple-silicon
macOS and Debian amd64/ARM64. New Mac users connect their Tesla account in the native
app, configure Hub without token files, install the embedded service package,
and start collection. Linux keeps full CLI/package operation. Support the legacy
Owner API and Tesla Fleet API, explicit wake/control commands, bounded TeslaMate
write-back, and retained provider raw JSON. TeslaFi import is not required.

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
2. Keep exactly one refresh-token owner: the resident service. Collection and
   commands must share it; never construct a command-only credential manager.
3. Keep normal backup data-only. Exclude decryption/signing keys. Offer key
   disaster recovery only as an explicit, separately encrypted export.
4. Integrate Tesla Auth-compatible PKCE login in the macOS app using native
   WebKit. Pass tokens to the embedded Rust CLI through bounded stdin, never
   files, argv, UI output, or logs. Configure before package install so the
   package can start a ready service.
5. Preserve compact TeslaMate v4.1.1 migration, macOS LaunchAgent operation,
   Debian systemd packaging, local sync, repair, and telemetry collection while
   extending every relevant identity and lifecycle path to multiple vehicles.
6. Add Fleet API collection and explicit wake/control commands through the same
   resident credential owner. Keep commands authenticated, bounded, auditable,
   and off unless directly requested.
7. Add explicit transactional TeslaMate write-back and provider raw JSON
   retention. Write-back is opt-in and must not run during ordinary migration.
8. Run final Rust/macOS checks once. Run Linux package/runtime proof only when
   Linux behavior or dependencies changed; reuse the one normal Cargo cache.

## Resource limits

- Work only in `hub/`; never edit or build `app/`.
- Use this checkout and `hub/target` only.
- Never create clones, worktrees, copied source trees, extra Cargo target
  directories, evidence archives, benchmark data, or long-lived VM images.
- Run one Cargo process at a time. Use focused tests while coding and one final
  suite/check/Clippy/release pass near handoff.
- If a disposable Linux guest is required, keep it below 6 GiB and delete it
  immediately after verification. For an unavoidable native remote build,
  transfer only the Hub source to one named `/var/tmp` directory; exclude
  targets and artifacts, then delete that exact directory after the `.deb` is
  copied back. Keep `CARGO_HOME` and `RUSTUP_HOME` inside that directory for
  that build command only; never edit shell profiles or leave a Cargo/Rustup
  path pointing at the disposable directory.
- Clean disposable debug/test artefacts before handoff; retain at most the
  normal release cache needed for the next developer.

## Safety

- TeslaMate PostgreSQL migration access remains read-only. A separate explicit
  write-back command may write only its declared bounded transaction.
- Never print or persist plaintext Tesla tokens outside the protected Hub
  credential store. Never place them in process arguments.
- Never send a real vehicle command during development or testing without the
  user's explicit live-test instruction. Hermetic fake-server tests are allowed.
- Never run two processes that can refresh the same token pair.
- Never expose the plaintext loopback listener through forwarding, proxying,
  tunnelling, or a non-loopback bind without TLS and authentication.
- Never push, publish, deploy, or change remote systems without explicit user
  direction. Preserve unrelated user files.

## Done

Done means the native macOS login/setup/install path works hermetically; default
backup excludes keys; encrypted credential recovery round-trips and rejects
wrong keys/tampering/overwrite; one refresh owner is enforced; multiple vehicles
collect and sync independently through legacy and Fleet sources; fake-server
wake/control tests pass; write-back is bounded and opt-in; provider raw JSON
round-trips; Rust format, tests, Clippy, and release build pass; the AppKit app
builds/tests; Linux still cross-checks or package-tests as appropriate; temporary
build/test data is removed; and `PROGRESS.md` separates completed proof from
deferred live commands, physical driving, clean-host install, iOS sync, and
endurance tests.
