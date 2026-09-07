# Product versioning

This repository carries the shared Teslatlas Hub ecosystem product version
`2026.36.2`. It is a candidate cohort number in `YEAR.WEEK.REVISION` form. No
tag, package publication, or compatibility result follows from the number.

The product version appears in `VERSION`, `pyproject.toml`, and the root package
entry in `uv.lock`. It does not replace protocol profiles `1.0.0`, `1.1.0`, or
`1.2.0`. The separate `hub-http-v1@1.0.0` identity will describe the current
Hub wire surface when its content-addressed profile is exported.

`compatibility/hub.json` remains a candidate with no tested Hub versions,
source fingerprints, profile hash, or receipts. Those fields must be populated
from real profile and integration evidence before compatibility can be claimed.
