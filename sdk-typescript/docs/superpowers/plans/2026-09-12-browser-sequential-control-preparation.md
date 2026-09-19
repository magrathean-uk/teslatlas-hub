# Browser Sequential Control Preparation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prepare one fresh, inert, directly supervised macOS browser-control root for a later single-attempt sequential trust run, without launching Chrome, importing a certificate, contacting Hub, or creating runtime evidence.

**Architecture:** A persistent Unix-socket supervisor will own both Chrome children and remain alive across the runner's CDP attach. It will spawn each child with `detached: false`, `shell: false`, piped/redacted stderr, exact profile paths, and exact port ownership; two small clients will request untrusted start/cleanup and trusted start/cleanup. The trust hook will own only keychain import/export/removal and will request trusted browser lifecycle operations from the supervisor.

**Tech Stack:** Node.js 26.7.0 on Apple silicon macOS, Node `child_process.spawn`/`execFile`, Unix domain sockets, Playwright CDP attachment performed only by the later SDK runner, and JSON preparation/runtime receipts with owner-only permissions.

**Spec:** Hub's 2026-09-12 delegation for fresh root `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2` using origin `http://localhost:4175`, trusted CDP `http://127.0.0.1:9234`, untrusted CDP `http://127.0.0.1:9235`, and a future Hub endpoint on port 18527.

## Global Constraints

- Preparation must not start Chrome, start the helper server, contact Hub, use port 18527, touch the keychain, or create certificate/profile/socket/runtime-evidence files.
- The root and its scripts must be wholly fresh; no r1/r2 prior tuple paths, profiles, state, evidence, browser processes, or certificate material may be reused.
- Browser children are always directly supervised with `detached: false`, `shell: false`, piped stderr, exact PID/lsof/command capture at attach, and error/exit/close/signal capture at teardown.
- The later run has one attempt and no retry; stale active state, an occupied expected port, or a second session must fail closed.
- Cleanup is exact and recoverable: only owned PIDs, profiles, sockets, logs, and named evidence paths may be removed; no broad process or directory deletion is permitted.
- Existing dirty checkout changes remain untouched apart from this preparation plan and its explicit preparation evidence.

### Task 1: Create the fresh control root and supervised lifecycle boundary

**Files:**
- Create: `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2/supervisor.mjs`
- Create: `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2/untrusted-control.mjs`

**Interfaces:**
- `supervisor.mjs serve` listens on the root Unix socket and accepts one JSON request per connection for `start-untrusted`, `cleanup-untrusted`, `start-trusted`, `cleanup-trusted`, and `shutdown`.
- The supervisor returns `{observed, endpoint, certificateSha256?, trustedCdpUrl, pid, attachSnapshot, events, closed?, portClosed?}` with exact binding checks and leaves a started child alive after the start response.
- `untrusted-control.mjs` accepts one JSON payload from the SDK runner, validates the fixed root/port/endpoint binding, forwards it to the supervisor, and prints one JSON response.

- [ ] **Step 1: Create only the fresh root and mode-0700 evidence directory.**

Run `test ! -e /Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2`, verify ports 4175, 9234, and 9235 are closed, then create the root and its empty mode-0700 `evidence` directory. Do not create profiles, state, socket, logs, certificate export, witness, or final receipt paths.

- [ ] **Step 2: Implement exact supervised child spawning.**

Use `spawn(chromePath, args, { detached: false, shell: false, stdio: ["ignore", "ignore", "pipe"] })`; use separate exact profile directories and ports 9235/9234; retain the child object as a direct child of the long-lived supervisor; attach `error`, `exit`, `close`, and redacted stderr listeners before waiting for the CDP endpoint.

- [ ] **Step 3: Implement attach and teardown snapshots.**

At CDP readiness capture `lsof -nP -iTCP:<port> -sTCP:LISTEN` and `ps -p <pid> -o pid=,ppid=,stat=,command=` with the root/profile path redacted only in persisted diagnostics. At teardown wait for `exit` and `close`, retain code and signal, verify the exact port is closed, and remove only the exact owned profile and runtime paths.

- [ ] **Step 4: Implement signal and stale-state cleanup.**

On `SIGINT`, `SIGTERM`, `SIGHUP`, uncaught exception, or unhandled rejection, terminate the exact active child, await its lifecycle events, verify its exact port is closed, remove its exact profile and socket, and exit with failure. Reject occupied ports, stale state, a second active child, mismatched operation bindings, and any retry/session number other than one.

- [ ] **Step 5: Syntax-check and inspect the static boundary.**

Run the pinned Node executable with `--check` on both modules and search the sources for `detached: true`, `shell: true`, broad process-kill commands, insecure-certificate flags, and reuse of the r1 root or prior ports. Expected result: no match and no runtime artifacts.

### Task 2: Implement keychain trust hooks over the supervised lifecycle

**Files:**
- Create: `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2/trust-hooks.mjs`

**Interfaces:**
- The hook receives one JSON argument from `browser-hub-runner.mjs` and supports `import`, `trusted-browser-cleanup`, `remove`, and `verify-removal`.
- `import` validates the exact future endpoint `https://localhost:18527/`, fresh certificate digest/path, expected export path, and trusted CDP `http://127.0.0.1:9234`; it runs the later authorized keychain operations with `execFile`, exports the certificate mode 0600, then requests `start-trusted` from the supervisor.
- `trusted-browser-cleanup` requests exact trusted-child cleanup even after a malformed import response or failed CDP attach, validates the fresh binding and closed port, and removes only the exact export/witness paths.
- `remove` uses the exact imported SHA-1 fingerprint against the login keychain; `verify-removal` uses `security find-certificate -a -p <login-keychain>` with the keychain as a positional argument and reports absence.

- [ ] **Step 1: Validate fresh bindings and canonical paths.**

Reject any endpoint other than `https://localhost:18527/`, any CDP URL other than `http://127.0.0.1:9234`, any non-absolute path outside the new root, any non-64-hex certificate SHA-256, and any pre-existing canonical export path.

- [ ] **Step 2: Encode shell-free keychain argv arrays.**

Use `execFile` with argv arrays only. Keep user-domain `security add-trusted-cert -r trustRoot -k <login-keychain> <certificatePath>` for this self-signed certificate, require `security verify-cert -p ssl -n localhost -k <login-keychain> -L` after import, and use `security find-certificate -a -p <login-keychain>` plus a fresh normal Chrome rejection for removal verification; never use a shell or `-k` with `find-certificate`.

- [ ] **Step 3: Bridge trusted lifecycle to the supervisor.**

After successful import/export, request `start-trusted` and return the supervisor's observed PID/attach data plus the exact export path, trust-store path, fingerprint, and trusted CDP URL. Cleanup must be callable independently and must return `closed: true` and `portClosed: true` for the fresh binding.

- [ ] **Step 4: Syntax-check and static security scan.**

Run the pinned Node `--check` and reject any shell invocation, insecure certificate bypass, broad kill, prior-root path, or trust-hook command that can run before the untrusted negative control is closed.

### Task 3: Record inert preparation evidence and verify the preflight

**Files:**
- Create: `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2/preparation.json`
- Create: `docs/development/macos-browser-sequential-control-preparation-2026-09-12-r2.json`
- Modify: `docs/development/STATUS.json` only if the existing preparation-status record requires the exact new hashes and closed-port preflight.

**Interfaces:**
- Both preparation records are mode 0600, contain the exact root/ports/commands/script SHA-256 values, say `prepared_not_run`, and explicitly list the absent runtime paths and prohibited actions.
- The repository record is a redacted, source-reviewable copy; it contains no keychain output, certificate material, credential, browser process state, or Hub handoff.

- [ ] **Step 1: Hash the three scripts and write the preparation manifest.**

Use the exact pinned Node path and absolute script paths in all future command arrays. Include the later runner command arrays, expected runtime paths, one-attempt/no-retry policy, and preflight observations without starting any command that launches Chrome or contacts Hub.

- [ ] **Step 2: Verify permissions and absence.**

Confirm root/evidence/script/manifest modes, 4175/9234/9235 closed, no Chrome process contains the new root or ports, and all profiles/socket/state/log/export/witness/final-receipt paths are absent.

- [ ] **Step 3: Run static checks only.**

Run Node syntax checks, Biome formatting/checking on the scripts where applicable, and focused `rg` scans for forbidden lifecycle/trust bypasses. Do not run the supervisor, control clients, trust hooks, helper server, SDK runner, or any keychain command.

- [ ] **Step 4: Persist and report the exact preparation boundary.**

Record script hashes, manifest hashes, modes, closed-port evidence, and the explicit negative statement that no Chrome, Hub endpoint, keychain, certificate, or runtime acceptance receipt was created. Leave the root ready for Hub's later fresh handoff and do not retry this attempt.

## Completion Review

Before reporting preparation complete, compare every requested tuple and path against the delegation, scan for placeholders and forbidden bypasses, verify every future command uses the new root and exact ports, and distinguish static preparation from the later live browser, trust, Hub, SDK-route, and acceptance gates.
