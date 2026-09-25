# Teslatlas Home Assistant integration

This repository owns the public-protocol Home Assistant bridge.

- Follow current Home Assistant integration conventions and quality requirements.
- Use the public Hub HTTP APIs and the supported bounded polling profile only;
  do not add SSE, event-stream, command, or private collector calls.
- Keep pairing, discovery, reauthentication, diagnostics, and redaction explicit.
- Commands are absent by default and require separately approved scopes.
- Do not access Hub storage, collector internals, or Tesla credentials.
- Preserve the independent `main` checkout and unrelated dirty files. Do not
  branch, reset, clean, stash, commit, push, publish, or submit to HACS without
  explicit authorization for that exact action.
- Keep local, synthetic, installed, UI, scheduler, replacement, and live
  evidence separate. A green local suite or composed launcher does not prove
  installed acceptance.
- GitHub is source storage only for this product; do not add CI, release,
  artifact-upload, or HACS automation.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.

