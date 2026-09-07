# Product versioning

SwiftPM has no package-version property. `VERSION` and the generated
`teslatlasProductVersion` constant therefore carry the shared ecosystem product
version `2026.36.2`. If a Swift repository source tag is later authorized, it
must use the same number.

The calendar product number does not alter `swift-tools-version`, richer
protocol profile `1.2.0`, the exact historical Hub v1 binding, or the
independent `hub-http-v1@1.0.0` wire revision. `TeslatlasCurrentHub` pins that
current profile to manifest SHA-256
`b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b`
and admits only its explicit tested Hub product versions.
