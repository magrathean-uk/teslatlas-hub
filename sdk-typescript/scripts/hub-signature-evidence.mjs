import { createPublicKey, verify } from "node:crypto";

const ed25519SpkiPrefix = Buffer.from("302a300506032b6570032100", "hex");

export function requireManifestPublicKey(discovery) {
  const value = discovery?.manifestPublicKey;
  if (typeof value !== "string" || !/^[0-9a-f]{64}$/u.test(value)) {
    throw new Error("sync discovery omitted its canonical manifestPublicKey");
  }
  return value;
}

export function verifyHubResponseSignature(bytes, signatureHeader, publicKeyHex) {
  if (!(bytes instanceof Uint8Array)) throw new Error("signed response bytes are invalid");
  if (typeof signatureHeader !== "string" || signatureHeader.length === 0) {
    throw new Error("signed response omitted its signature");
  }
  if (!/^[0-9a-f]{64}$/u.test(publicKeyHex)) throw new Error("manifest public key is invalid");
  const signature = Buffer.from(signatureHeader, "base64");
  if (signature.byteLength !== 64 || signature.toString("base64") !== signatureHeader) {
    throw new Error("response signature encoding is invalid");
  }
  const publicKey = createPublicKey({
    format: "der",
    key: Buffer.concat([ed25519SpkiPrefix, Buffer.from(publicKeyHex, "hex")]),
    type: "spki",
  });
  if (!verify(null, bytes, publicKey, signature)) {
    throw new Error("response signature does not verify over the exact body bytes");
  }
  return { signatureBytes: signature.byteLength, verified: true };
}
