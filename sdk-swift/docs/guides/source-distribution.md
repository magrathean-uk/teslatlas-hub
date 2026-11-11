# Source distribution

`tools/source_handoff.py` prepares and verifies a deterministic source handoff for all four public libraries. It is a source package, not a binary release or runtime acceptance artifact. Run these commands from a full repository checkout.

## Temporary validation

```sh
python3 tools/source_handoff.py exercise
```

The command stages the package under the required basename `teslatlas-sdk-swift`, verifies the file manifest, asks SwiftPM to parse the package and builds a separate consumer importing all four libraries. It does not run that consumer. The temporary package, consumer and build scratch are removed on exit, including failure.

## Retained handoff

Choose an absent output directory outside the checkout:

```sh
python3 tools/source_handoff.py prepare --output /tmp/teslatlas-sdk-review-handoff
python3 tools/source_handoff.py verify --handoff /tmp/teslatlas-sdk-review-handoff
python3 tools/source_handoff.py smoke --handoff /tmp/teslatlas-sdk-review-handoff
```

`prepare` refuses an existing path. The result contains `source-handoff.json`, the package directory and a sibling `external-four-library-consumer`. `smoke` builds that separate consumer with external scratch, removes known SwiftPM metadata it generated and verifies the handoff again.

## Contents and integrity

The selection in `tools/source_handoff.py` includes the manifest, version, license, unchanged NOTICE, README, package sources and tests, both source examples and seven named public guides. It excludes Git metadata, build output, private inputs, historical receipts, platform harness projects and the `tools` directory. Additional repository policies and new documentation are not included automatically. Links to repository-only material in a handoff should be read in the full source checkout.

The manifest binds ordered relative paths, byte sizes and SHA-256 digests. Verification rejects duplicate JSON keys, unexpected schema or entries, symlinks, special files, path escapes, checksum changes and an invalid four-product consumer. Directories use mode `0755`, files `0644`, and timestamps `2000-01-01T00:00:00Z`.

Documentation edits change the current-tree handoff identity when those documents are selected. They do not change the older identity pinned by the platform-gate harness. Do not overwrite old receipts or claim that their results cover the new package.

Current handoffs require NOTICE. The platform harness reconstructs its older pinned member list from the pinned tree, independently of the current checkout, and explicitly permits that historical handoff's earlier selection. Its iOS lane stages historical host and test inputs and binds the package reference to that accepted handoff. This preserves the older pin and distinguishes historical checks from current-source acceptance.

Execution-bearing `smoke` and `exercise` commands use an owned command budget: 60 seconds for manifest inspection, 900 seconds for a build, at most 8 MiB combined captured output, a 5 second TERM grace and a 5 second KILL/reap deadline. Timeout, excess output and cleanup failures block success. These are tooling limits, not measured build performance or proof of OS process or Docker resource settlement. Use the workspace's authorized Clean Development routing and heavy-build lock for applicable runs.

A successful smoke check proves package parsing and consumer compilation on the recorded host. It does not establish minimum OS support, live Hub behavior, installation, upgrade, rollback or real-data acceptance.
