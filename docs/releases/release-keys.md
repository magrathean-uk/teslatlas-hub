# Historical release-key notes

Teslatlas Hub v1.0.0 uses an annotated source tag. It has no GitHub Release,
downloadable release assets, checksum bundle, or active release-key workflow.
The tag itself is the public source boundary.

Earlier prerelease work explored separate release, provenance, and Debian
attestation identities. Those experiments remain visible in Git history and in
the evidence tooling for reproducibility, but they are not trust requirements
or publication claims for v1.0.0.

Verify the current source identity directly:

```sh
git fetch --tags origin
test "$(git cat-file -t v1.0.0)" = tag
git show --no-patch --format=fuller v1.0.0
git checkout --detach v1.0.0
test -z "$(git status --porcelain=v1 --untracked-files=all)"
```

The expected tagger is György Bolyki using the repository's documented GitHub
address. Compare the checked-out commit with the revision named by the project
documentation before building locally.
