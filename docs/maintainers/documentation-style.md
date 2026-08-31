# Documentation style

Documentation is part of the product. It must describe the exact supported
release or observable source behaviour, not an intended future state.

## Write for action

- State the audience, outcome and support boundary in the opening paragraph.
- Put the safe path first; move edge cases and rationale after the working path.
- Prefer short sentences, concrete nouns and direct verbs.
- Use UK English except for code, API names, quoted upstream terms and proper
  names.
- Avoid slogans, filler, inflated claims, unexplained acronyms and generic
  feature copy.

## Commands and configuration

- Commands must be copyable, use real paths or visibly explicit placeholders,
  and show the required user or privilege boundary.
- Never place credentials, VINs, coordinates or realistic personal telemetry in
  examples.
- State prerequisites, expected success evidence, failure points and rollback
  where a command can mutate data or system state.
- Link to one authoritative procedure instead of copying a long sequence into
  several pages.

## Accuracy and versions

- Distinguish a tagged release from `main` and do not claim support from an
  untested platform or moving branch.
- Tie version, platform, dependency and provider claims to source, release
  evidence or upstream documentation.
- Use `30 August 2026`-style dates in prose and ISO 8601 where a
  machine-readable value is required.
- Mark assumptions and unresolved facts. Do not turn a private record, generated
  report or administrative classification into proof of legal ownership.

## Legal and project identity

- Use **Teslatlas**, **Teslatlas Hub**, **György Bolyki** and
  **MAGRATHEAN UK LTD** exactly as written.
- Describe creator credit as fact, not as an advertising condition or implied
  endorsement.
- Keep AGPL obligations, third-party licences, copyright notices and trade-mark
  policy separate.
- Do not add legal restrictions, warranty promises or support commitments in a
  guide. Route them to the controlling policy.

## Visual material

- Keep editable project-owned source beside generated raster output.
- Provide useful alternative text and make diagrams readable without colour
  alone.
- Do not copy Tesla, TeslaMate or another third party's logo or trade dress.
- Use diagrams only where they reduce explanation; decorative clutter is not
  documentation.

Before merge, run the repository layout and provenance verifiers and check all
local links from the file's actual location.
