# Product versioning

This repository carries the shared Teslatlas Hub ecosystem product version
`2026.36.2` in `YEAR.WEEK.REVISION` form. The number alone does not imply a
tag, package publication, release artifact, or support.

The product version appears in `VERSION`, `pyproject.toml`, and the root package
entry in `uv.lock`. It does not replace the current-Hub HTTP profile
`hub-http-v1@1.3.0` (with its retained predecessor `1.2.0`), the Hub sync profiles, or the receiver envelope profile
`receiver-v1@1.0.0`. These identities are selected independently.

The current-Hub profile is exported at `profiles/hub-http-v1/1.3.0/` (the retained
predecessor at `profiles/hub-http-v1/1.2.0/`). Its
`version` discovery field reports the Hub's product version; its `profile`
member advertises the profile identity. Its OpenAPI 3.1.0 document uses
adjacent schemas, so consumers must bind the complete profile directory by its
manifest digest.

The served Hub sync contracts `hub-sync-v1@1.3.0` and `hub-sync-v1@1.4.0`
have independent revisions. `hub-sync-v1@1.5.0` is a candidate and is not served.
HTTP `1.2.0` is retired as an advertised revision; its frozen bytes retain
the historical `status: served` claim, while its keys remain aliases in `1.3.0`. Historical admission does not establish current runtime support; see
[verification limits](verification.md).
