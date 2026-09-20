import { createHash } from "node:crypto";
import { chmod, readFile, writeFile } from "node:fs/promises";
import { requireManifestPublicKey, verifyHubResponseSignature } from "./hub-signature-evidence.mjs";
import {
  requestSignedSyncJson,
  schema22SyncHeaders,
  validateSchema22Noop,
} from "./hub-sync-contract-evidence.mjs";

const descriptor = JSON.parse(await readFile(required("TESLATLAS_HUB_HTTP_CONFIG"), "utf8"));
const credential = JSON.parse(await readFile(required("TESLATLAS_HUB_CREDENTIAL_FILE"), "utf8"));
const receiptPath = required("TESLATLAS_HUB_SYNC_RECEIPT");
validateCredential(credential);

const endpoint = new URL(descriptor.endpoint);
const discovery = await jsonRequest("/.well-known/teslatlas-hub");
if (discovery.body.hub_id !== descriptor.hub_id) throw new Error("Hub identity differs");
if (!discovery.body.capabilities?.includes("sync.packs")) {
  throw new Error("Hub does not advertise sync.packs");
}
const manifestPublicKey = requireManifestPublicKey(discovery.body);

const authorization = { Authorization: `Bearer ${credential.accessToken}` };
const vehicles = await jsonRequest("/v1/vehicles", { headers: authorization });
const vehicleId = vehicles.body.vehicles?.[0]?.vehicle_id;
if (typeof vehicleId !== "string") throw new Error("Hub returned no vehicle for sync checks");
const manifest = await requestSignedSyncJson({
  authorization,
  endpoint,
  path: `/v1/vehicles/${encodeURIComponent(vehicleId)}/sync/manifest`,
  signaturePublicKey: manifestPublicKey,
});
const pack = findPackDescriptor(manifest.body);
if (pack === undefined) throw new Error("sync manifest did not contain an authorized pack");
if (!/^\/v1\/packs\/sha256\/[0-9a-f]{64}\.sqlite\.zst$/u.test(pack.relative_path)) {
  throw new Error("sync manifest pack path is not canonical");
}
if (!pack.relative_path.includes(pack.sha256)) {
  throw new Error("sync manifest pack path does not match its digest");
}

const noop = await requestSignedSyncJson({
  authorization,
  endpoint,
  path: `/v1/vehicles/${encodeURIComponent(vehicleId)}/sync/noop`,
  signaturePublicKey: manifestPublicKey,
});
validateSchema22Noop(manifest.body, noop.body);
const packResponse = await fetch(new URL(pack.relative_path, endpoint), {
  headers: schema22SyncHeaders(authorization),
  redirect: "error",
});
if (packResponse.status !== 200) throw new Error(`pack returned HTTP ${packResponse.status}`);
const contentLength = numberHeader(packResponse, "content-length");
if (contentLength !== pack.compressed_bytes || contentLength > 64 * 1024 * 1024) {
  throw new Error("pack content length differs from its bounded manifest descriptor");
}
const packBytes = new Uint8Array(await packResponse.arrayBuffer());
if (packBytes.byteLength !== contentLength) throw new Error("pack stream length differs");
const packSha256 = createHash("sha256").update(packBytes).digest("hex");
if (packSha256 !== pack.sha256) throw new Error("pack stream digest differs");

const unsupported = {};
for (const path of ["/v1/events", "/v1/data-quality"]) {
  const response = await fetch(new URL(path, endpoint), {
    headers: authorization,
    redirect: "error",
  });
  if (response.status !== 404) throw new Error(`${path} unexpectedly returned ${response.status}`);
  await response.arrayBuffer();
  unsupported[path] = response.status;
}

const receipt = {
  schemaVersion: 1,
  result: "MAC3_HUB_SYNC_CONTRACT_PASS",
  hubId: discovery.body.hub_id,
  vehicleId,
  manifest: {
    status: manifest.status,
    bodyBytes: manifest.bodyBytes,
    bodySha256: manifest.bodySha256,
    cacheControl: manifest.cacheControl,
    packCount: countPackDescriptors(manifest.body),
    signatureBytes: manifest.signatureBytes,
    signaturePresent: manifest.signaturePresent,
    signatureVerified: manifest.signatureVerified,
  },
  noop: {
    bodyBytes: noop.bodyBytes,
    bodySha256: noop.bodySha256,
    status: noop.status,
    cacheControl: noop.cacheControl,
    signatureBytes: noop.signatureBytes,
    signaturePresent: noop.signaturePresent,
    signatureVerified: noop.signatureVerified,
  },
  pack: {
    status: packResponse.status,
    sha256: packSha256,
    bytes: packBytes.byteLength,
    etag: packResponse.headers.get("etag"),
    cacheControl: packResponse.headers.get("cache-control"),
  },
  unsupported,
};
await writeFile(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, {
  mode: 0o600,
  flag: "wx",
});
await chmod(receiptPath, 0o600);
process.stdout.write(`${JSON.stringify(receipt)}\n`);

async function jsonRequest(path, { headers, signaturePublicKey } = {}) {
  const response = await fetch(new URL(path, endpoint), { headers, redirect: "error" });
  if (response.status !== 200) throw new Error(`${path} returned HTTP ${response.status}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength === 0 || bytes.byteLength > 4 * 1024 * 1024) {
    throw new Error(`${path} returned an invalid bounded JSON body`);
  }
  let body;
  try {
    body = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new Error(`${path} returned invalid JSON`);
  }
  const signature = response.headers.get("x-teslatlas-manifest-signature");
  const signatureEvidence =
    signaturePublicKey === undefined
      ? { signatureBytes: 0, verified: false }
      : verifyHubResponseSignature(bytes, signature, signaturePublicKey);
  return {
    body,
    bodyBytes: bytes.byteLength,
    bodySha256: createHash("sha256").update(bytes).digest("hex"),
    cacheControl: response.headers.get("cache-control"),
    signatureBytes: signatureEvidence.signatureBytes,
    signaturePresent: signature !== null,
    signatureVerified: signatureEvidence.verified,
    status: response.status,
  };
}

function findPackDescriptor(value) {
  if (value === null || typeof value !== "object") return undefined;
  if (
    typeof value.relative_path === "string" &&
    typeof value.sha256 === "string" &&
    Number.isSafeInteger(value.compressed_bytes)
  ) {
    return value;
  }
  for (const child of Array.isArray(value) ? value : Object.values(value)) {
    const found = findPackDescriptor(child);
    if (found !== undefined) return found;
  }
  return undefined;
}

function countPackDescriptors(value) {
  if (value === null || typeof value !== "object") return 0;
  const self =
    typeof value.relative_path === "string" &&
    typeof value.sha256 === "string" &&
    Number.isSafeInteger(value.compressed_bytes)
      ? 1
      : 0;
  return (
    self +
    (Array.isArray(value) ? value : Object.values(value)).reduce(
      (count, child) => count + countPackDescriptors(child),
      0,
    )
  );
}

function numberHeader(response, name) {
  const value = response.headers.get(name);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) throw new Error(`${name} is invalid`);
  return parsed;
}

function validateCredential(value) {
  if (
    value === null ||
    typeof value !== "object" ||
    !/^[0-9a-f]{64}$/u.test(value.accessToken) ||
    typeof value.deviceId !== "string" ||
    !Number.isSafeInteger(value.expiresAtMs) ||
    value.expiresAtMs <= Date.now()
  ) {
    throw new Error("credential file is invalid or expired");
  }
}

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}
