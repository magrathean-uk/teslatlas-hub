# Teslatlas Home Assistant integration

This repository owns the public-protocol Home Assistant bridge.

- Follow current Home Assistant integration conventions and quality requirements.
- Use public Hub APIs and event streams only.
- Keep pairing, discovery, reauthentication, diagnostics, and redaction explicit.
- Commands are absent by default and require separately approved scopes.
- Do not access Hub storage, collector internals, or Tesla credentials.
