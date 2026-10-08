export function requireManifestPublicKey(discovery: unknown): string;

export function verifyHubResponseSignature(
  bytes: Uint8Array,
  signatureHeader: string | null,
  publicKeyHex: string,
): { readonly signatureBytes: 64; readonly verified: true };
