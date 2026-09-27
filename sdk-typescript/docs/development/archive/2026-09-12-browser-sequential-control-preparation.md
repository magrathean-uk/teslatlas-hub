# Historical browser sequential-control preparation

This 2026-09-12 plan described one disposable browser trust-control attempt. Its runtime paths, ports, credentials, and one-attempt authorization are closed historical context. It is not an instruction to recreate or execute that attempt.

The design required a directly supervised browser process, shell-free control messages, normal certificate validation, an untrusted control before trust import, and exact cleanup of owned processes and trust material. Those principles remain relevant; the former host-specific commands are not a reusable setup guide.

Use the [development record](../../development/PLAN.md) for authority, [compatibility](../../compatibility.md) for the SDK boundary, and the [Firefox NSS helper guide](../../macos-firefox-nss-browser.md) for the separate implemented profile-local trust route. Any real-Hub run needs a fresh handoff. The original preparation plan remains available in repository history.
