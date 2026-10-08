import { generateKeyPairSync, sign } from "node:crypto";
import { describe, expect, it } from "vitest";
import {
  requireManifestPublicKey,
  verifyHubResponseSignature,
} from "../../scripts/hub-signature-evidence.mjs";

describe("Hub signature evidence", () => {
  it("verifies Ed25519 over the exact downloaded response bytes", () => {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const publicDer = publicKey.export({ format: "der", type: "spki" });
    const publicKeyHex = publicDer.subarray(-32).toString("hex");
    const bytes = Buffer.from('{"schema":{"major":2,"minor":2}}\n');
    const signature = sign(null, bytes, privateKey).toString("base64");

    expect(requireManifestPublicKey({ manifestPublicKey: publicKeyHex })).toBe(publicKeyHex);
    expect(verifyHubResponseSignature(bytes, signature, publicKeyHex)).toEqual({
      signatureBytes: 64,
      verified: true,
    });
    expect(() =>
      verifyHubResponseSignature(Buffer.from(`${bytes} `), signature, publicKeyHex),
    ).toThrow("exact body bytes");
  });

  it("rejects aliases, missing signatures, and malformed signature encodings", () => {
    const publicKeyHex = "a".repeat(64);

    expect(() => requireManifestPublicKey({ manifest_public_key: publicKeyHex })).toThrow(
      "canonical manifestPublicKey",
    );
    expect(() => verifyHubResponseSignature(new Uint8Array(), null, publicKeyHex)).toThrow(
      "omitted its signature",
    );
    expect(() => verifyHubResponseSignature(new Uint8Array(), "not-base64", publicKeyHex)).toThrow(
      "encoding is invalid",
    );
  });
});
