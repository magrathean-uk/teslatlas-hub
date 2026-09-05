# Release versioning

Distribution policy: Hub is source-only. Do not create GitHub Releases or
upload prebuilt packages. Ordinary fixes are pushed without creating a new
release or tag; version changes require an explicit decision. Existing tags
remain immutable. See [source publishing](releasing.md).

Accepted on 5 September 2026. Future Teslatlas Hub releases use the
calendar format **`YEAR.WEEK.REVISION`**.

The first agreed calendar version is **`2026.36.1`**, with Git tag
**`v2026.36.1`**. This records the versioning decision, not a published release
or a claim that existing binaries already contain that version.

## Choose a version

- `YEAR` is the ISO week-numbering year, and `WEEK` is its ISO week number
  (1–53). Weeks begin on Monday. Use the Europe/London release date when
  allocating a new release version.
- `REVISION` starts at 1 for a new week and increases for every subsequent
  public release allocated in that week, including hotfixes.
- Write numbers without leading zeros: `2026.36.1`, not `2026.09.05`.
  The latter resembles a year/month/day date, which is not this policy.
- Compare the three components numerically, not as strings. For example,
  `2026.36.10` is newer than `2026.36.9`.
- Never reuse a public release version, move a published tag, or overwrite
  published binaries with different bytes. Withdrawn versions remain reserved.
- If an unpublished release is delayed into another week, allocate a version
  for the new week and rebuild. The initial agreed version above applies to
  the planned week-36 release, not indefinitely to the next release.

| Version | Meaning | Next example |
|---|---|---|
| `2026.36.1` | First release in week 36 | `2026.36.2` for another release that week |
| `2026.37.1` | First release in week 37 | `2026.37.2` for a hotfix that week |
| `2027.1.1` | First release in ISO week 1 of 2027 | `2027.1.2` for another release that week |

At a year boundary, use the ISO week-year, not the calendar year: 1 January
2027 belongs to ISO week 53 of 2026; 4 January begins ISO week 1 of 2027.

## Keep source and packages aligned

Use one product version across the Rust CLI, macOS app, macOS installer,
Debian amd64 and ARM64 packages, release notes, and Git tag. The Git tag adds
only a `v` prefix; the release title is `Teslatlas Hub 2026.36.1`.

The three numeric components fit Cargo's version syntax, but their meaning is
calendar-based rather than a semantic-version compatibility promise. Update
`Cargo.toml`, the root package entry in `Cargo.lock`, macOS product-version
metadata, installer inputs, affected tests, and current documentation together.
Do not rewrite historical release notes or historical fixtures merely because
they contain an older version.

Platform-specific metadata remains separate:

- macOS internal build numbers identify rebuilds and must increase according
  to the platform's rules. They do not replace the displayed product version.
- A Debian packaging suffix such as `2026.36.1-1` is a package revision, not
  an extra component of the Hub product version.
- Internal rebuilds may retain an unpublished product version while changing
  their build identifier. Once distributed publicly, changed binaries require
  a new product release version and matching source, notes, and checksums.

## Preserve history and compatibility

Keep the existing `v1.0.0` and prerelease tags unchanged. Calendar versioning
supersedes them for future Hub releases; it does not rename old releases or
authorize attaching newer binaries to their tags. This policy applies to Hub,
not automatically to the separate Teslatlas app repository.

Document database migrations, API and sync compatibility, minimum supported
platforms, upgrade/rollback restrictions, and breaking changes explicitly in
each release's notes. The date-based number alone guarantees none of these.

## Release checklist

1. Inspect existing local and remote tags and releases before allocating a
   version; ensure the new version is unused and newer than the previous one.
2. Apply the same product version to source and all intended platform builds.
3. Rebuild and verify the actual embedded versions; never just rename an old
   package. Record the source commit, build identifiers, hashes, and test results.
4. Keep platform limitations, signing/notarisation status, and unavailable
   features explicit. A numbering change does not waive release gates.
5. Tag the verified source commit and publish only its matching, verified
   artifacts when publication is authorised. Do not overwrite historical tags
   or assets, and do not upload private logs, credentials, or backups.

See the [release process](releasing.md) for the historical source-only workflow
and the boundary between that workflow and future downloadable releases.
