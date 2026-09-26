# Synthetic TLS test fixture

`localhost-cert.pem` and `localhost-key.pem` are a self-signed certificate and private key kept for loopback unit tests. They are test inputs only. They are not a Hub certificate, a deployment trust anchor, a credential, or a supported way to configure TLS in an application.

`tests/unit/node-hub-pinning.test.ts` loads these files to start a local HTTPS server. The test supplies the certificate as the caller-owned Node CA, computes the connected leaf certificate DER SHA-256 pin, and checks successful pairing, pin mismatch, missing-pin validation, and ordinary trust failure. The tests also assert that a rejected claim does not send the claim secret.

Do not copy these files into an application, use them to trust a remote endpoint, or include them in a package or deployment. Keep all real certificates, keys, invitations, and credentials outside the repository.
