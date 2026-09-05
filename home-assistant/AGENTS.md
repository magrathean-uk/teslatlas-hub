# Teslatlas Home Assistant integration

This repository owns the public-protocol Home Assistant bridge.

- Follow current Home Assistant integration conventions and quality requirements.
- Use public Hub APIs and event streams only.
- Keep pairing, discovery, reauthentication, diagnostics, and redaction explicit.
- Commands are absent by default and require separately approved scopes.
- Do not access Hub storage, collector internals, or Tesla credentials.

## GPT-6 Astra execution

Reference: [OpenAI GPT-6 Astra guide](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra), reviewed 2026-09-05.
These execution conventions preserve the repository-specific rules above and do
not change the host's model defaults or API configuration.

- Infer the intended objective and scope, then carry authorized work through to
  completion. Resolve routine choices with judgment; ask focused questions only
  when material uncertainty affects the result, scope, or required authority.
- Retain the user's session permissions and preferences. Complete authorized work
  that makes the result concrete and reviewable before a necessary final
  approval; explain the specific remaining approval requirement.
- Follow current user directions over skill guidelines within higher-priority
  instructions and tool constraints. If a skill blocks progress, link the exact
  `SKILL.md`, quote its relevant rule, and explain how it applies.
- Treat later messages as steering the current objective unless the user
  explicitly cancels it or replaces it with an incompatible objective.
- Delegate bounded independent work to available agents when useful work can
  continue in parallel. Assign file ownership to avoid shared edit races. Batch
  independent reads; keep dependent operations and conflicting edits sequential.
- Scale meaningful verification to the change. Once relevant checks pass, stop
  repeating or broadening them unless new changes, failures, or unresolved
  concerns justify it. Preserve required repository gates.
- Report failed, blocked, and untested paths accurately. Separate inspection,
  local tests, simulator evidence, and real-device or live-service acceptance
  where applicable; compilation alone does not prove acceptance.
- Use concise, plain, outcome-first prose and meaningful progress updates.
  Explain what changed, why, the supporting evidence, and material limits.
  Keep messages between agents legible, with normal spacing.
