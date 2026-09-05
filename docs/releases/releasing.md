# Source publishing and local builds

As of 5 September 2026, Hub is source-only. Do not create GitHub Releases or
upload installer assets. Users build their own packages using
[Build from source](../guides/build-from-source.md).

## Publish source changes

1. Review the diff and preserve unrelated work.
2. Run tests relevant to changed components and the repository's layout and
   provenance checks. Record exact completed results.
3. Commit and push to `main` only when authorised. Do not upload `dist/`,
   credentials, private logs or databases.
4. Do not create a release or tag as part of an ordinary fix. Existing tags
   remain immutable historical source snapshots.

GitHub stores source only. Build and test on controlled local hosts, not
GitHub Actions. The [calendar versioning policy](versioning.md) still controls
embedded product versions when a version change is explicitly requested; it
does not require a GitHub Release.

## Build and distribute independently

Use the [source build guide](../guides/build-from-source.md) for macOS and
Debian. Retain the source commit, toolchain versions, package scope,
checksums and signing status for your own builds. A successful build does not
prove live collection or restored-backup acceptance.

Anyone redistributing binaries remains responsible for the complete
[corresponding source](../legal/source-availability.md), dependency notices,
and build/install material required by the licence. Local builds do not
automatically gain Developer ID signing, notarisation or trusted in-app updates.

Historical release notes describe their original artifacts, not current
download availability. Removing GitHub release pages does not remove source
tags or alter previously distributed bytes.
