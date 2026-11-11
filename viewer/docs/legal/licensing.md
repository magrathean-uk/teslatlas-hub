# Licensing

Teslatlas Viewer is licensed under the unmodified
[Apache License, Version 2.0](../../LICENSE), which controls.

## Repository record

The root `LICENSE` contains the complete Apache License 2.0 text. The root
[NOTICE](../../NOTICE) file records the copyright holder. `package.json` marks
the package as private and states no licence expression. Source files carry no
file-level SPDX identifiers.

Keep the complete root `LICENSE` and `NOTICE` with every source distribution.

## Third-party components

`npm run build` bundles the packages below into the static files in `dist/`.
Each licence is the one declared in that package's own manifest.

React and its scheduler are installed at the versions fixed in this
repository's `package-lock.json`:

| Package | Locked version | Declared licence |
| --- | --- | --- |
| react | 19.3.0 | MIT |
| react-dom | 19.3.0 | MIT |
| scheduler | 0.28.0 | MIT |

The Viewer imports the browser entry point of the Teslatlas TypeScript SDK,
installed from the pinned archive recorded in `artifacts/teslatlas-sdk.json`.
That entry point already contains Ajv and its runtime dependencies, bundled when
the archive was built. The archive's `package.json` matches the SDK repository
at commit `e97be68`, and the versions below are those in the SDK's
`package-lock.json` at that commit:

| Package | Locked version | Declared licence |
| --- | --- | --- |
| @teslatlas/sdk | 2026.36.2 | Apache-2.0 |
| ajv | 8.20.0 | MIT |
| ajv-formats | 3.0.1 | MIT |
| fast-deep-equal | 3.1.3 | MIT |
| fast-uri | 3.1.8 | BSD-3-Clause |
| json-schema-traverse | 1.0.0 | MIT |
| require-from-string | 2.0.2 | MIT |

When you distribute the built files or the container image, include the
licence text and copyright notice of each package above, taken from that
package's source at the listed version.

The Docker image is built on the official Node.js 26.10.0 Debian slim image
pinned in the [Dockerfile](../../Dockerfile). Node.js and the Debian packages in
that image carry their own licences, some of them copyleft. A distributed image
must meet those terms as well.

The test and build tools (Vite, Vitest, Playwright, Testing Library, jsdom,
TypeScript, axe-core and their dependencies) are not bundled into `dist/`, except
for the small module-preload and runtime helper code that the Vite build, through
Vite and Rolldown (both MIT), writes into the output. Some of the tools are under
other licences, including MPL-2.0 for axe-core and Lightning CSS.
