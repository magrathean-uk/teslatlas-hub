# Licensing

The Teslatlas TypeScript SDK is licensed under the Apache License, Version
2.0. The complete, controlling licence text is in the repository root at
[`LICENSE`](../../LICENSE). The package metadata identifies the same licence as
`Apache-2.0`.

[`NOTICE`](../../NOTICE) at the repository root records the copyright holder and
the licence. Keep the root licence text and `NOTICE` unchanged when
distributing source or package artifacts. The Apache License includes
conditions for retaining applicable copyright, patent, trademark, and
attribution notices.

## Third-party components

The SDK's runtime dependency graph, at the exact versions fixed in
`package-lock.json`, is listed below. Each licence is the one declared in that
package's own manifest. The Node entry points load these packages from
`node_modules`. The browser entry point, `dist/browser.js`, contains a bundled
copy of Ajv and the packages it uses.

| Package | Locked version | Declared licence |
| --- | --- | --- |
| ajv | 8.20.0 | MIT |
| ajv-formats | 3.0.1 | MIT |
| fast-deep-equal | 3.1.3 | MIT |
| fast-uri | 3.1.8 | BSD-3-Clause |
| json-schema-traverse | 1.0.0 | MIT |
| require-from-string | 2.0.2 | MIT |

When you distribute a package, a browser bundle or an application that
contains the SDK, include the licence text and copyright notice of each package
above, taken from that package's source at the listed version.

The development tools in `devDependencies` (Biome, TypeScript, Vite, Vitest,
Playwright and their dependencies) are not part of the package. Some are under
other licences, including MPL-2.0 for Lightning CSS, which Vite uses. They are not
bundled into `dist/`, except for the small runtime helper code that the Vite
build, through Rolldown (MIT), writes into `dist/browser.js`.
