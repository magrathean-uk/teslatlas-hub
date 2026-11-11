# Product versioning

SwiftPM has no package-version property. `VERSION` and the generated
`teslatlasProductVersion` constant therefore carry the shared ecosystem product
version `2026.36.2`. If a Swift repository source tag is later authorized, it
must use the same number.

The calendar product number does not alter `swift-tools-version`, richer
protocol profile `1.2.0`, the exact historical Hub v1 binding, or the
independent `hub-http-v1@1.0.0` wire revision. `TeslatlasCurrentHub` pins that
current profile to manifest SHA-256
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`
and admits only the explicit Hub product version recorded by its binding. A
local build or fixture run does not by itself establish installed acceptance.
