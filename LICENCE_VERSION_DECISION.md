# Licence-version decision

Decision date: 17 August 2026

Current Hub baseline: `a2b8431028abb8d84465196fceb0c951de901cee`

## Decision

New authorised releases from the current baseline use:

```text
AGPL-3.0-only
```

## Reason

The combined work includes compatibility material whose reviewed upstream licence grant establishes GNU AGPL version 3. The `only` selection avoids implying authority to offer every relevant part under an unknown future version.

## Earlier grants

Any release already conveyed as `AGPL-3.0-or-later` remains available under that grant. The Company cannot revoke a compliant recipient's existing licence.

## Consistency requirement

Before release, verify the same expression appears in:

- Cargo metadata;
- README/licensing documents;
- `NOTICE`;
- CLI legal output;
- GUI About/Legal;
- package metadata;
- source headers;
- release notes.

Current `main` was observed to use `AGPL-3.0-only` in Cargo, runtime legal output and the macOS service-details panel.
