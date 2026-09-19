# Azure Debian v1 evidence contract

**Status:** prepared final-bootstrap source contract; not an Azure provisioning request, installation record, or release authorization. Azure is used only after all other development/integration is complete and the existing Mac plus VPS verification has completed at minimum. Hub then coordinates execution after the owner supplies two fresh disposable Azure Debian virtual machines: one ARM64 and one x86_64. It does not defer, replace, or substitute for Mac or VPS tests.

## Identity lock

Hub must admit the exact HA packet before either run. Immediately before each lane, Hub records that packet's actual file SHA-256 in the redacted receipt, then records these stable source and runtime identities verbatim:

| Identity | Required value |
| --- | --- |
| Ten-file source manifest | `e74a955374003be9820a450174715412b4a9c1d504b47eb33d1f93e6b2173832` |
| Component manifest | `0a4ec2d743a3f181423c24bbf7c6d9d7e0feef5dc0f0fc3e5d3d69150945e896` |
| Embedded profile `SHA256SUMS` | `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926` |
| Provenance binding | `33cda376023bfff2b1f2361abc52bc110bdd21387b15a712c2d7dafebc49415e` |
| Expected HA runtime | Home Assistant `2026.8.3`, Python `3.14.2` |
| Candidate image reference | `ghcr.io/home-assistant/home-assistant:2026.8.3` |

The owner records the resolved immutable image digest, reported image architecture, Debian release, kernel, Docker Engine and Compose versions before installation. A tag, VM label, or architecture request does not substitute for observed values. If the resolved image, component bytes, profile, Python, or Home Assistant version differs, Hub stops that lane, records the mismatch, and obtains a newly admitted identity before proceeding.

## Two disposable lanes

| Lane | Required target proof | Result scope |
| --- | --- | --- |
| `azure-debian-arm64-v1` | Fresh Azure Debian VM reports ARM64 architecture; owner-provided disposable lifecycle control; usable Linux Docker Engine and Compose | One admitted 18-case HA cell plus Container evidence below |
| `azure-debian-x86_64-v1` | Fresh Azure Debian VM reports x86_64 architecture; owner-provided disposable lifecycle control; usable Linux Docker Engine and Compose | One admitted 18-case HA cell plus Container evidence below |

Hub records the redacted target identifier and time window, not credentials, invitations, CA material, IP addresses, bearer values, or VM access tokens. These two lanes do not replace the separately required macOS ARM64 Hub target.

## Ordered evidence protocol

1. **Preflight and install.** Start from an empty disposable VM. Capture Docker/Compose availability, the exact image digest and architecture, the source/component/profile identity lock, persistent configuration location and a private trust setup. Verify the component mount contains the approved component before starting it. Do not accept a silently created empty mount.
2. **Activation.** Pair through the Home Assistant UI with a short-lived Hub-issued invitation and trusted TLS. Record the redacted config-entry outcome, Hub identity/profile confirmation and no secret material. A hand-authored storage record does not count as activation.
3. **Function.** Run the admitted 18-case matrix against the real Hub target and separately observe a changed sensor value, a normal scheduled poll, unavailable/recovery behavior, reauthentication after revocation, reload and unload. Retain the Hub-controller result and HA-side redacted logs/receipts for each required case.
4. **Restart and persistence.** Restart the HA container, then recreate it while preserving `/config`. Confirm the entry ID, entity IDs, custom names and disabled flags survive, then repeat a successful authenticated poll. Record actual HA database/config persistence and clean container startup; a running process alone is insufficient.
5. **Upgrade.** Back up the disposable `/config` state. Record the previous image tag/digest and selected replacement tag/digest, apply the owner-approved image update, recreate the container and repeat activation-state and function observations. If any identity in the lock changes, do not carry the prior result forward: bind a new manifest and rerun the affected acceptance scope.
6. **Uninstall and cleanup.** After receipts are retained, unload and stop only the test-owned HA resources, verify Hub-side transport/client cleanup and no child process remains, then remove the disposable Container installation. Record whether persistent test configuration was deleted. Do not remove user-owned resources, revoke unrelated credentials, or claim cleanup from an unobserved VM teardown.

## Required receipt fields

Each lane produces one redacted receipt containing: target/architecture and observed host facts; the current admitted packet SHA-256, the six stable identity-lock values plus image digest; preflight result; installation and UI activation evidence; all 18 admitted case outcomes; changed-poll, normal-scheduler, outage/recovery, revocation/reauth, reload and unload observations; restart/recreation and persistence evidence; upgrade before/after identities and result; final stop, transport cleanup, uninstall outcome and retained evidence locations. Failures, skips, missing inputs and incomplete cleanup remain explicit.

Hub owns VM access, registry admission, private inputs, controller evidence and execution. This repository owns this contract and may bind receipts into public compatibility metadata only after Hub independently admits both lanes. The owner must not be asked for Azure access early; the final Azure work begins only after Hub records the prerequisite Mac and VPS evidence.
