# Teslatlas Hub v1.0.0-beta.2

Unpublished source candidate, prepared 2026-08-31.

This is not a GitHub release. It has no package, release artifact, signed
publication, notarisation record, or complete object-code source offer. Do not
install it as a supported release.

## Changes since beta.1

- Guided TeslaMate migration requires a trusted OpenSSH known-host entry before
  it supplies SSH authentication or reads database credentials.
- Guided TeslaMate migration requires TeslaMate 4.2.0 or newer.
- The running candidate links to the public source repository instead of a
  nonexistent beta.2 release page.
- Stream, listener, and companion shutdown remains bounded and owned by Hub.

## Evaluation only

Evaluate from the repository source after reviewing local changes and running
the documented checks. Keep TeslaMate and Hub from concurrently owning the
same legacy refresh-token pair. Back up data before migration or upgrade.

The "v1.0.0-beta.1" tag and documentation remain historical records. They do
not make this beta.2 candidate an official release.
