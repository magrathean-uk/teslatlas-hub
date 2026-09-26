# Support

Identify the product before investigating a problem:

| Product | Contract to check |
| --- | --- |
| `TeslatlasHubSDK` and `TeslatlasCommands` | Public protocol profiles through 1.2.0 |
| `TeslatlasHubV1Compatibility` | Exact historical Hub v1.0.0 binding |
| `TeslatlasCurrentHub` | Bundled `hub-http-v1@1.0.0` profile and admitted Hub product version |

A server may expose more routes than the selected client contract. Check [current Hub](docs/current-hub.md), [historical compatibility](docs/hub-v1-compatibility.md) and [versioning](docs/product-versioning.md) before changing validation to accept a response.

For non-sensitive bugs and questions, use the [repository issues](https://github.com/magrathean-uk/teslatlas-sdk-swift/issues).

## Prepare a useful report

Include the SDK revision, library, Swift version, operating system, Hub product/profile version, a small synthetic reproduction, expected behavior and the typed error. Include the command and result if a local check fails. Redact tokens, invitations, private certificates, Hub and vehicle identifiers, location data and personal paths.

For a suspected vulnerability, follow [SECURITY.md](SECURITY.md) and keep exploit details out of public reports. No response-time or supported-release commitment is made here.
