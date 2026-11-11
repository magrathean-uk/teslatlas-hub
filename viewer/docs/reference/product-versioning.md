# Product versioning

The npm manifest, lockfile, and [build metadata](../../public/version.json) record Viewer version `2026.36.2`. This identifies the source candidate's product cohort. It does not prove publication, installed acceptance, or Hub compatibility.

[compatibility/hub.json](../../compatibility/hub.json) binds the candidate to `hub-http-v1@1.0.0` and its profile digest. It remains `candidate`, with empty tested Hub versions, source fingerprints, and receipt arrays. Read that file for the exact digest rather than inferring compatibility from the product version.

[artifacts/teslatlas-sdk.json](../../artifacts/teslatlas-sdk.json) separately binds the SDK package, source commit, preparation toolchain, archive hash, installed content, and profile. A version change alone cannot replace those integrity checks.

Keep these identities consistent during any future authorized update, and record the exact source and artifacts behind new compatibility evidence. Existing historical receipts do not automatically apply to a rebuilt package.
