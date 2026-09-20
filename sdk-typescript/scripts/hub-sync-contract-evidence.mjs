import { createHash } from "node:crypto";
import { verifyHubResponseSignature } from "./hub-signature-evidence.mjs";

export const HUB_SYNC_SCHEMA = "2.2";
export const HUB_SYNC_SCHEMA_HEADER = "x-teslatlas-supported-schemas";

export function schema22SyncHeaders(headers = {}) {
  return { ...headers, [HUB_SYNC_SCHEMA_HEADER]: HUB_SYNC_SCHEMA };
}

export async function requestSignedSyncJson({
  endpoint,
  path,
  authorization,
  signaturePublicKey,
  fetch = globalThis.fetch,
}) {
  const response = await fetch(new URL(path, endpoint), {
    headers: schema22SyncHeaders(authorization),
    redirect: "error",
  });
  if (response.status !== 200) throw new Error(`${path} returned HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength === 0 || bytes.byteLength > 4 * 1024 * 1024) {
    throw new Error(`${path} returned an invalid bounded JSON body`);
  }
  const signature = response.headers.get("x-teslatlas-manifest-signature");
  const signatureEvidence = verifyHubResponseSignature(bytes, signature, signaturePublicKey);
  let body;
  try {
    body = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new Error(`${path} returned invalid JSON`);
  }
  return {
    body,
    bodyBytes: bytes.byteLength,
    bodySha256: createHash("sha256").update(bytes).digest("hex"),
    cacheControl: response.headers.get("cache-control"),
    signatureBytes: signatureEvidence.signatureBytes,
    signaturePresent: true,
    signatureVerified: signatureEvidence.verified,
    status: response.status,
  };
}

export function validateSchema22Noop(manifest, noop) {
  if (!isRecord(manifest) || !isRecord(noop)) {
    throw new Error("schema 2.2 manifest/no-op pair is malformed");
  }
  const chunks = manifest.chunks;
  if (!Array.isArray(chunks) || chunks.length !== 1 || !isRecord(chunks[0])) {
    throw new Error("schema 2.2 manifest/no-op pair is malformed");
  }
  const pack = chunks[0];
  const requiredStrings = [
    "installation_id",
    "account_id",
    "vehicle_id",
    "snapshot_id",
    "terminal_cursor",
  ];
  const requiredIntegers = ["generation", "head_sequence"];
  if (
    noop.schema !== "teslatlas-hub-schema-22-noop-v1" ||
    noop.projection_schema !== "2.2" ||
    requiredStrings.some((field) => typeof noop[field] !== "string" || noop[field].length === 0) ||
    requiredIntegers.some((field) => !Number.isSafeInteger(noop[field]) || noop[field] < 0) ||
    typeof noop.pack_sha256 !== "string" ||
    !/^[0-9a-f]{64}$/u.test(noop.pack_sha256)
  ) {
    throw new Error("schema 2.2 no-op response is malformed");
  }
  for (const field of [...requiredStrings, ...requiredIntegers]) {
    if (noop[field] !== manifest[field]) {
      throw new Error("schema 2.2 manifest/no-op pair does not match");
    }
  }
  if (noop.pack_sha256 !== pack.sha256) {
    throw new Error("schema 2.2 manifest/no-op pair does not match");
  }
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
