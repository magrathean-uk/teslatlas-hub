# Teslatlas Viewer Reference Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an accessible, responsive, fixture-backed open reference client that exercises every currently documented public Teslatlas Hub client surface without inventing live routes.

**Architecture:** A Vite/React/TypeScript single-page app consumes an app-owned `ViewerDataSource` boundary. `FixtureDataSource` supplies deterministic demonstrations of discovery, pairing, health, vehicles, current state, recent drives and charges, data quality, collector freshness, and paired devices. Because the public protocol and TypeScript SDK are foundation-only, non-fixture mode returns an explicit unavailable state instead of defining a speculative public transport API.

**Tech Stack:** React, TypeScript, Vite, Vitest, Testing Library, Playwright, axe-core, plain CSS.

**Spec:** `docs/architecture.md` and `docs/plans/2026-08-30-foundation.md`

## Global Constraints

- Call public SDK APIs only; the current SDK has no runtime API, so make no live Hub request.
- Use only public protocol concepts and label viewer-owned fixture fields as non-contract examples.
- Keep scope to discovery/pairing, health, vehicles, current state, recent sessions, data quality, freshness, and paired devices.
- Show loading, empty, stale, inferred, degraded, offline, and error states distinctly in text, not colour alone.
- Do not add commands, proprietary Teslatlas analytics/UI, private Hub routes, hosted deployment, GitHub Actions, Dependabot, or release automation.
- Keep credentials in memory in fixture mode; do not invent browser persistence policy.
- Preserve deterministic redacted data: no real VIN, token, precise location, or user identity.

---

### Task 1: Toolchain and test harness

**Files:**
- Create: `package.json`, `package-lock.json`, `index.html`, `tsconfig.json`, `tsconfig.app.json`, `tsconfig.node.json`, `vite.config.ts`, `vitest.setup.ts`
- Create: `playwright.config.ts`

**Interfaces:**
- Produces: `npm run dev`, `npm run build`, `npm run typecheck`, `npm test`, and `npm run test:e2e`.

- [x] Add the package scripts and dependencies for React/Vite, Vitest/jsdom, Testing Library, Playwright, and axe-core.
- [x] Configure Vitest for jsdom with `vitest.setup.ts` importing `@testing-library/jest-dom/vitest`.
- [x] Configure Playwright to start `npm run dev -- --host 127.0.0.1`, use a fixed UTC timezone, and write artifacts under `test-results/`.
- [x] Run `npm install` and `npm run typecheck`; expected result is an empty-project configuration check without errors.

### Task 2: Internal data boundary and deterministic fixtures

**Files:**
- Test: `src/data/fixture-data-source.test.ts`
- Create: `src/data/types.ts`, `src/data/fixture-data.ts`, `src/data/fixture-data-source.ts`, `src/data/create-data-source.ts`

**Interfaces:**
- Produces: `FixtureScenario = 'complete' | 'empty' | 'stale' | 'inferred' | 'degraded' | 'offline' | 'error' | 'loading'`.
- Produces: `ViewerDataSource.discover(signal): Promise<DiscoveredHub[]>`, `pair(input, signal): Promise<PairedHub>`, `readSnapshot(scenario, signal): Promise<HubSnapshot>`, and `removePairedDevice(deviceId, signal): Promise<PairedDevice[]>`.
- Produces: `createDataSource(mode: 'fixture' | 'live'): ViewerDataSource`; live mode rejects with `SDK_NOT_RELEASED` before networking.

- [x] Write tests with hand-derived expectations for complete, empty, stale, inferred, degraded, offline, error, loading, successful fixture pairing, invalid invitation code, device removal, and unavailable live mode.
- [x] Run `npm test -- src/data/fixture-data-source.test.ts`; expected failure is unresolved data-source modules.
- [x] Implement source-neutral viewer types and fixed redacted fixture data timestamped `2026-08-30T10:00:00Z`.
- [x] Implement the fixture and unavailable live sources. The loading scenario remains pending until aborted; all others resolve deterministically without timers.
- [x] Run the focused test; expected result is all data-source tests passing.

### Task 3: App state, discovery, and pairing

**Files:**
- Test: `src/app/App.test.tsx`
- Create: `src/main.tsx`, `src/app/App.tsx`, `src/app/use-viewer.ts`, `src/components/connection-panel.tsx`

**Interfaces:**
- Consumes: `ViewerDataSource` from Task 2.
- Produces: `<App dataSource initialScenario initialPaired />` for deterministic tests and browser scenarios.
- Produces: URL inputs `mode=fixture|live`, `scenario=<FixtureScenario>`, and `paired=false`.

- [x] Write tests proving the app announces loading, discovers a fixture Hub, pairs with invitation `482731`, rejects another code, exposes the pinned identity, and explains that live SDK support is unavailable without issuing a request.
- [x] Run `npm test -- src/app/App.test.tsx`; expected failure is unresolved app modules.
- [x] Implement the abort-safe state hook, skip link, semantic header/main/navigation shell, and accessible pairing form.
- [x] Run the focused test; expected result is all app/pairing tests passing.

### Task 4: Seven views and explicit data states

**Files:**
- Test: `src/views/views.test.tsx`
- Create: `src/components/data-state.tsx`, `src/components/status-pill.tsx`, `src/components/metric.tsx`, `src/components/view-frame.tsx`
- Create: `src/views/hub-health-view.tsx`, `src/views/vehicles-view.tsx`, `src/views/current-state-view.tsx`, `src/views/recent-sessions-view.tsx`, `src/views/data-quality-view.tsx`, `src/views/collector-freshness-view.tsx`, `src/views/paired-devices-view.tsx`
- Create: `src/styles.css`

**Interfaces:**
- Consumes: immutable `HubSnapshot` values and callbacks from `App`.
- Produces: seven labelled navigation targets and state markers `data-view-state="loading|empty|stale|inferred|degraded|offline|error|complete"`.

- [x] Write role-based tests for every view, navigation keyboard operation, empty collections, absent current fields, stale timestamps, inferred fields, degraded gaps, offline collectors, source errors, and paired-device removal.
- [x] Run `npm test -- src/views/views.test.tsx`; expected failure is unresolved view modules.
- [x] Implement shared semantic state components and the seven focused views.
- [x] Implement mobile-first CSS with visible focus, high-contrast state labels, reduced motion, reflow at 400% zoom, and no colour-only status meaning.
- [x] Run focused and full unit tests; expected result is all unit tests passing.

### Task 5: Browser and accessibility verification

**Files:**
- Create: `e2e/viewer.spec.ts`, `e2e/accessibility.spec.ts`, `e2e/screenshots.spec.ts`
- Create after capture: `output/playwright/screenshots/complete-desktop.png`, `output/playwright/screenshots/complete-mobile.png`, `output/playwright/screenshots/stale-desktop.png`, `output/playwright/screenshots/error-mobile.png`, `output/playwright/screenshots/pairing-mobile.png`

**Interfaces:**
- Consumes: deterministic query inputs from Task 3.
- Produces: browser assertions and stable screenshots at 1280x800 and 375x812.

- [x] Write Playwright tests that traverse all seven views, exercise complete/empty/stale/inferred/degraded/offline/error states, complete pairing, remove a device, and assert fixture mode makes no HTTP API request.
- [x] Add axe scans, keyboard focus checks, reduced-motion checks, and 400% reflow coverage.
- [x] Run `npm run test:e2e`; fix only after a failing browser assertion or accessibility violation is observed.
- [x] Capture the five deterministic screenshots with animations disabled and wait on `data-view-state` markers.

### Task 6: Contributor documentation and release evidence

**Files:**
- Modify: `README.md`, `docs/architecture.md`
- Create: `docs/protocol-learning-guide.md`, `docs/privacy.md`, `docs/browser-support.md`

**Interfaces:**
- Documents: fixture/live boundary, exercised public concepts, setup, tests, state semantics, privacy, browser support, and SDK integration seam.

- [x] Document clean-checkout setup and every verification command.
- [x] State that fixture payloads are viewer-owned examples, not released protocol schemas, and live mode stays disabled until released SDK artifacts exist.
- [x] Map each view to public protocol candidate resources without presenting exact unfrozen routes or fields as compatible contracts.
- [x] Run `rg -n "TODO|TBD|/v1/|commands" README.md docs src e2e`; review every match and remove speculative routes, placeholders, or command UI.
- [x] Run `npm run typecheck`, `npm test -- --run`, `npm run build`, and `npm run test:e2e` fresh.
- [x] Ask a read-only reviewer to audit scope, accessibility, public-contract boundaries, tests, and screenshots; address supported findings.
- [x] Check `git diff --check`, review the complete diff, make one coherent commit, and push `main` once.
