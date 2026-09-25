# Workspace planning snapshot — 2026-09-19

The parent workspace has no working Git repository. This source-only snapshot keeps
its current authority, plans, unsent goal, history summary and development wrappers
in the Hub repository. Product source and product plans remain in their own repositories.

Paths in SNAPSHOT_MANIFEST.json are relative to the workspace root. To restore, first
inspect the manifest and merge the files into the workspace alongside the six product
checkouts; do not blindly overwrite newer local work. Preserve wrapper executable modes.
Markdown links retain their original workspace context and resolve after restoration.

The bundled UPLOAD_STATUS.json records preparation, before this commit existed. Final
remote HEADs are recorded in the live root copy after the six pushes. Original receipt
HEADs and dirty-tree fingerprints remain historical runtime evidence, not claims that
these snapshot commits received new runtime acceptance.

The next goal is docs/development/GOAL_PROMPT.md. It is written but not sent or started.
No App/Viewer files, private runtime inputs or raw transcripts are included. Archived
automation settings are intentionally omitted; their original local files are retained.
