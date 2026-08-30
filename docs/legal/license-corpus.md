# Canonical dependency licence texts

Some Rust packages declare a standard SPDX licence expression but omit the
corresponding licence text from their crate archive. Release evidence must not
invent or silently omit that text.

`LICENSES/` contains only the canonical texts needed by the locked dependency
graph when a package-local file is absent. The files were retrieved from the
official SPDX `license-list-data` repository at revision:

```text
a3cbf2e897d54bccec0c35469c691521d089ef53
```

| SPDX identifier | SHA-256 |
|---|---|
| `Apache-2.0` | `074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff` |
| `BSL-1.0` | `84c6ef3ea9e3254a54d0acf5d3e0c61ae011b8fef7dd6940591cf060e6380a8f` |
| `LGPL-2.1-or-later` | `5749785c8bdefafcb5d798270ed0a967036fe2ca63dcedade1627565dfef81d2` |
| `MIT` | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `Zlib` | `bfb1112d49db5b1daecdfef24bd7e2f3ea0bafb33aa67aa0ab51e2bf8407c03d` |

Package-local licence files take precedence. Corpus fallback is allowed only
for a validated SPDX identifier present in the dependency expression and fails
closed when the required canonical file is missing or unsafe.

The corpus does not change a dependency's licence, select among alternatives,
or replace package-specific notices.
