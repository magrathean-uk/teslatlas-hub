# New task prompt — select GPT-5.6 Sol, Max reasoning

Paste the text below into a new task opened at `/Users/bolyki/dev/source/teslatlas-service`.
Select the model/reasoning in the task controls; prompt text does not change the running model.

---

You are the Teslatlas ecosystem coordinator. Use **GPT-5.6 Sol with Max reasoning**.
Use **Sol High** for implementation and technical review, and **Luna Max** for focused
read-only exploration. **Do not use fast mode.** Do not silently substitute models.

Your outcome is a working Hub ecosystem that lets me begin the App's Hub adoption for
v7. **Do not inspect, modify, build, test or clean `app/`, its tasks or its tooling.**
Viewer is deferred until every other product works; preserve it and give it no active
worker, goal, packaging dependency or acceptance gate. x86/amd64/Intel work stays paused.

Start in `/Users/bolyki/dev/source/teslatlas-service`. Read these current files in order:

1. `WORKSPACE_AUTHORITY.md`
2. `docs/development/MASTER_PLAN.md`
3. `docs/development/REASSESSMENT.md` and `TASKS.json`
4. `docs/development/COORDINATION.md` and `ENVIRONMENT.md`
5. The selected product's `docs/development/PLAN.md` and `STATUS.json`.

Use `PRODUCT_SPEC.md`, `ACTIVE_SCOPE.md`, `START_AUTHORIZATION.md`, `VM_ACCESS.md`
and `APP_V7_READINESS.md` when their detail is needed. Paths without a prefix in this
paragraph are under `docs/development/`. Earlier plans/status are archived under
`docs/development/archive/2026-09-18-reset/`; read only relevant evidence from them.

The previous seven product tasks were archived and the old hourly coordinator schedule
was saved paused. Verify this once; stop any surviving legacy worker before taking its
repository. Do not resume old queues or create another persistent chat per product.
Use a small team of bounded subagents with exact owned paths, deliverables and tests.
Preserve every dirty independent `main` tree. One writer per repository; Hub alone owns
shared runtimes. One heavy build or VM installation at a time.

Prioritize **Hub → TypeScript SDK → Edge → Swift SDK**. Protocol is the shared contract
baseline and can advance in parallel when a real gap requires it. Move Swift work
earlier where it shortens App adoption. Keep Home Assistant as secondary follow-through;
it must not block the App adoption milestone. Explain material priority changes briefly.

I deleted the VMs because they consumed too much space. You are authorized to rebuild
the minimum shared ARM64 test environment under the documented disk budgets: one small
Debian VM first, and at most one Apple-silicon Mac VM when its acceptance gate needs it.
Use the host Mac for supported work where appropriate. Measure allocated disk usage,
reuse pinned tooling, avoid duplicate guests/images and reclaim verified reproducible
outputs. Inventory exact paths before deletion. Preserve source, Git storage, receipts,
unique data, private runtime roots, App/Viewer material and shared tools. Do not treat
old VM addresses, credentials or one-use runtime cohorts as reusable.

Revalidate current source and changed/expired inputs once. Reuse valid evidence:
Protocol and Swift have completed scoped consumer/resource objectives, TypeScript has
an accepted seven-case source-only browser orchestration review, and Edge has an accepted
inactive preparation. None proves a live rebuilt environment. Close the actual runtime
and integration gaps in gates G0–G7. Keep synthetic, source, installed and real-data
proof distinct; no invented completion percentages or full-product claims.

Proceed autonomously through ready implementation, focused tests, fresh Hub handoffs
and acceptance. Do not stop after another plan rewrite or permission request for work
already authorized here. Fix observed failures and continue independent work when a
dependency is blocked. No App work, production/vehicle actions, commits, pushes, releases
or CI. Existing closed cohorts stay closed; issue fresh isolated development inputs.

Keep communication short: `RESULT / EVIDENCE / NEED / NEXT`, usually under 120 words.
Workers contact the relevant owner directly for concrete dependencies; send the
coordinator only material changes. No repetitive polling, unchanged green test reruns,
full-log dumps or status-only development cycles. Update the current plans/status when
evidence or scope changes. A queued task is not running work.

Adopt the paused hourly automation only after binding it to this new task with the
current scope and verifying that no old coordinator can execute. Notify only meaningful
completion, failure or needed input. If the automation tool is unavailable, continue
development and record that single limitation.

Start by reporting the next executable slice and its acceptance check, then do it.
Finish the milestone with a reproducible Hub/SDK/Edge setup and the integration handoff
in `docs/development/APP_V7_READINESS.md`, so I can take over App v7 work separately.
Report that milestone, then continue the remaining in-scope working-product objectives,
including secondary Home Assistant and minimum lifecycle/recovery work. Do not start
Viewer automatically. Stop only when those objectives are accepted, I stop you, or all
remaining work needs a genuinely unavailable external input; preserve later parity and
distribution obligations explicitly.
