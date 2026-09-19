# Synthetic TLS fixture

`localhost-cert.pem` and `localhost-key.pem` are a public, test-only self-signed
pair for loopback unit tests. The key is not used by any Hub, environment, or
deployed service and must never be treated as trust or credential material.
