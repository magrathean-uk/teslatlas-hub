# Protocol-gated Swift SDK Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. One main-session writer owns all edits; subagents are read-only reviewers.

**Goal:** Create a tested, importable Swift package with contract-neutral SSE, persistence, and range-resume mechanics while recording the exact released-protocol dependency gate.

**Architecture:** Export one `TeslatlasHubSDK` library without invented public protocol declarations. Keep wire mechanics internal and independently tested so released protocol-derived models can bind to them later without copying App or Hub code.

**Tech Stack:** Swift 6, Swift Package Manager, Foundation, XCTest, Xcode 27 beta for generic iOS validation.

**Spec:** `docs/superpowers/specs/2026-08-30-protocol-gated-foundation-design.md`

## Global Constraints

- `teslatlas-protocol` is the only contract authority.
- Protocol authority is pinned for this batch to commit `b7b48a86a7705e8ab016f1debd25cecd20ebbb89`.
- Add no public protocol fields, endpoint paths, error codes, signature algorithms, retry limits, or compatibility claims.
- Read no proprietary App code and no AGPL Hub implementation.
- Add no GitHub Actions, hosted CI, Dependabot, release automation, or registry publication.
- Keep one writer and make one coherent final commit and push.

---

### Task 1: Package scaffold and SSE framing

**Files:**
- Create: `Package.swift`
- Create: `Sources/TeslatlasHubSDK/TeslatlasHubSDK.swift`
- Create: `Sources/TeslatlasHubSDK/ServerSentEventDecoder.swift`
- Create: `Tests/TeslatlasHubSDKTests/ServerSentEventDecoderTests.swift`

**Interfaces:**
- Produces internal `ServerSentEvent`, `ServerSentEventDecoder.append(_:)`, and `ServerSentEventDecoder.finish()`.

- [ ] Write tests that feed split literal UTF-8 chunks and assert multiline data, opaque IDs, retry parsing, comments, CRLF, and blank-line dispatch.
- [ ] Run `swift test --filter ServerSentEventDecoderTests`; expected failure is missing decoder symbols.
- [ ] Implement the smallest incremental line buffer and field accumulator that passes the tests.
- [ ] Run `swift test --filter ServerSentEventDecoderTests`; expected result is zero failures.

### Task 2: Atomic generic persistence

**Files:**
- Create: `Sources/TeslatlasHubSDK/AtomicJSONStateStore.swift`
- Create: `Tests/TeslatlasHubSDKTests/AtomicJSONStateStoreTests.swift`

**Interfaces:**
- Produces internal actor `AtomicJSONStateStore<State>` with `load()`, `save(_:)`, and `remove()` where `State: Codable & Sendable`.

- [ ] Write tests using a literal test state and a unique temporary directory; assert reload through a fresh store and idempotent removal.
- [ ] Run `swift test --filter AtomicJSONStateStoreTests`; expected failure is missing store symbols.
- [ ] Implement JSON encode/decode, parent-directory creation, atomic write, and idempotent removal.
- [ ] Run `swift test --filter AtomicJSONStateStoreTests`; expected result is zero failures.

### Task 3: Byte-range resume validation

**Files:**
- Create: `Sources/TeslatlasHubSDK/RangeResumeValidator.swift`
- Create: `Tests/TeslatlasHubSDKTests/RangeResumeValidatorTests.swift`

**Interfaces:**
- Produces internal `RangeResumeValidator.validate(statusCode:contentRange:eTag:requestedOffset:expectedETag:)` returning a parsed `ValidatedContentRange` or throwing `RangeResumeValidationError`.

- [ ] Write literal tests for a matching `bytes 10-19/100` response and rejection of status 200, wrong starts, malformed ranges, impossible totals, and changed ETags.
- [ ] Run `swift test --filter RangeResumeValidatorTests`; expected failure is missing validator symbols.
- [ ] Implement strict parsing and validation without adding endpoint, retry, digest, or manifest rules.
- [ ] Run `swift test --filter RangeResumeValidatorTests`; expected result is zero failures.

### Task 4: Documentation and standalone example

**Files:**
- Create: `.gitignore`
- Create: `Examples/TeslatlasHubSDKExample/main.swift`
- Create: `docs/protocol-dependency-gate.md`
- Modify: `README.md`

**Interfaces:**
- Produces the `TeslatlasHubSDKExample` executable and exact gate documentation.

- [ ] Add an executable target that imports `TeslatlasHubSDK` and prints the protocol authority commit plus blocked artifact classes.
- [ ] Document completed internal mechanics, forbidden invented surface, exact missing artifacts, and activation criteria.
- [ ] Run `swift run TeslatlasHubSDKExample`; expected output identifies the active gate and does not claim Hub connectivity.

### Task 5: Verification, review, and bulk publication

**Files:**
- Modify only files required by validated reviewer findings.

**Interfaces:**
- Consumes all prior tasks; produces one reviewed commit on `main`.

- [ ] Run `swift package dump-package`, `swift build`, `swift test`, and `swift run TeslatlasHubSDKExample` with fresh output.
- [ ] Run the generic iOS build with `/Applications/Xcode-beta.app` and capture the exact result.
- [ ] Dispatch a read-only reviewer with the starting and ending diffs; fix every Critical or Important issue using a failing regression test first.
- [ ] Re-run the full validation set after review.
- [ ] Confirm `git diff --check`, forbidden-import searches, current branch, and dirty-state scope.
- [ ] Create one coherent commit and push `main` once.
