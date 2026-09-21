import { createHash } from "node:crypto";
import { verifyHubResponseSignature } from "./hub-signature-evidence.mjs";

export const HUB_SYNC_SCHEMA = "2.2";
export const HUB_SYNC_SCHEMA_HEADER = "x-teslatlas-supported-schemas";

const SHA256 = /^[0-9a-f]{64}$/u;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u;
const HUB_PROJECTION_TABLES = new Set([
  "car",
  "drive",
  "position",
  "charge",
  "charge_sample",
  "state",
  "update",
  "tombstone",
]);

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
  const pack = validateSchema22Manifest(manifest);
  validateSchema22NoopShape(noop);
  for (const field of [
    "installation_id",
    "account_id",
    "vehicle_id",
    "generation",
    "snapshot_id",
    "head_sequence",
    "terminal_cursor",
  ]) {
    if (noop[field] !== manifest[field]) {
      throw new Error("schema 2.2 manifest/no-op pair does not match");
    }
  }
  if (noop.pack_sha256 !== pack.sha256) {
    throw new Error("schema 2.2 manifest/no-op pair does not match");
  }
}

function validateSchema22Manifest(manifest) {
  if (
    !isRecord(manifest) ||
    !isVersion(manifest.protocol, 1, 0) ||
    !isVersion(manifest.schema, 2, 2) ||
    !isUuid(manifest.installation_id) ||
    !isUuid(manifest.account_id) ||
    !isUuid(manifest.vehicle_id) ||
    !isPositiveInteger(manifest.generation) ||
    !isUuid(manifest.snapshot_id) ||
    manifest.mode !== "full_snapshot" ||
    !isNonNegativeInteger(manifest.base_sequence) ||
    !isNonNegativeInteger(manifest.head_sequence) ||
    manifest.base_sequence > manifest.head_sequence ||
    manifest.chunk_count !== 1 ||
    !isPositiveInteger(manifest.total_compressed_bytes) ||
    !isPositiveInteger(manifest.total_uncompressed_bytes) ||
    !isNonNegativeInteger(manifest.total_rows) ||
    !isOpaqueCursor(manifest.terminal_cursor) ||
    !Array.isArray(manifest.chunks) ||
    manifest.chunks.length !== 1
  ) {
    throw new Error("schema 2.2 manifest is malformed");
  }
  const pack = manifest.chunks[0];
  if (
    !isRecord(pack) ||
    !isUuid(pack.pack_id) ||
    pack.snapshot_id !== manifest.snapshot_id ||
    pack.ordinal !== 0 ||
    !isVersion(pack.schema, 2, 2) ||
    pack.format !== "hub_projection_sqlite" ||
    pack.compression !== "zstd" ||
    typeof pack.sha256 !== "string" ||
    !SHA256.test(pack.sha256) ||
    pack.relative_path !== `/v1/packs/sha256/${pack.sha256}.sqlite.zst` ||
    !isPositiveInteger(pack.compressed_bytes) ||
    !isPositiveInteger(pack.uncompressed_bytes) ||
    !isNonNegativeInteger(pack.row_count) ||
    !isRecord(pack.sequence) ||
    pack.sequence.from_exclusive !== manifest.base_sequence ||
    pack.sequence.to_inclusive !== manifest.head_sequence ||
    !Array.isArray(pack.tables) ||
    pack.tables.length === 0 ||
    pack.tables.length > HUB_PROJECTION_TABLES.size ||
    pack.tables.some((table) => typeof table !== "string" || !HUB_PROJECTION_TABLES.has(table)) ||
    new Set(pack.tables).size !== pack.tables.length ||
    manifest.total_compressed_bytes !== pack.compressed_bytes ||
    manifest.total_uncompressed_bytes !== pack.uncompressed_bytes ||
    manifest.total_rows !== pack.row_count
  ) {
    throw new Error("schema 2.2 manifest pack descriptor is malformed");
  }
  return pack;
}

function validateSchema22NoopShape(noop) {
  if (
    !isRecord(noop) ||
    noop.schema !== "teslatlas-hub-schema-22-noop-v1" ||
    noop.projection_schema !== HUB_SYNC_SCHEMA ||
    !isUuid(noop.installation_id) ||
    !isUuid(noop.account_id) ||
    !isUuid(noop.vehicle_id) ||
    !isPositiveInteger(noop.generation) ||
    !isUuid(noop.snapshot_id) ||
    !isNonNegativeInteger(noop.head_sequence) ||
    typeof noop.pack_sha256 !== "string" ||
    !SHA256.test(noop.pack_sha256) ||
    !isOpaqueCursor(noop.terminal_cursor)
  ) {
    throw new Error("schema 2.2 no-op response is malformed");
  }
}

function isVersion(value, major, minor) {
  return (
    isRecord(value) &&
    value.major === major &&
    value.minor === minor &&
    Object.keys(value).length === 2
  );
}

function isUuid(value) {
  return (
    typeof value === "string" &&
    UUID.test(value) &&
    value !== "00000000-0000-0000-0000-000000000000"
  );
}

function isPositiveInteger(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function isNonNegativeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function isOpaqueCursor(value) {
  return typeof value === "string" && value.length > 0;
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
