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
  const response = await fetchResponseWithDeadline(
    new URL(path, endpoint),
    {
      headers: schema22SyncHeaders(authorization),
      redirect: "error",
    },
    fetch,
  );
  const bytes = await readBoundedResponseBytes(response, {
    label: path,
    maxBytes: 4 * 1024 * 1024,
  });
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

export async function fetchResponseWithDeadline(
  input,
  init,
  fetch = globalThis.fetch,
  timeoutMs = 30_000,
) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 2_147_483_647) {
    throw new Error("invalid fetch deadline");
  }
  const controller = new AbortController();
  let timedOut = false;
  let timer;
  try {
    const response = Promise.resolve()
      .then(() => fetch(input, { ...init, signal: controller.signal }))
      .then((value) => {
        if (timedOut) void value.body?.cancel().catch(() => undefined);
        return value;
      });
    return await Promise.race([
      response,
      new Promise((_, reject) => {
        timer = setTimeout(() => {
          timedOut = true;
          controller.abort();
          reject(new Error("evidence fetch timed out"));
        }, timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

// Admit headers before reading and retain no bytes beyond the caller's cap.
// Cancellation is initiated without awaiting a peer-controlled cancel promise.
export async function readBoundedResponseBytes(
  response,
  { label, maxBytes, expectedBytes, requireContentLength = false, timeoutMs = 30_000 },
) {
  let reader;
  let timer;
  try {
    if (
      !Number.isSafeInteger(maxBytes) ||
      maxBytes < 1 ||
      (expectedBytes !== undefined &&
        (!Number.isSafeInteger(expectedBytes) || expectedBytes < 1 || expectedBytes > maxBytes)) ||
      !Number.isSafeInteger(timeoutMs) ||
      timeoutMs < 1 ||
      timeoutMs > 2_147_483_647
    ) {
      throw new Error("invalid bounded response limits");
    }
    if (response.status !== 200) throw new Error(`${label} returned HTTP ${response.status}`);
    const header = response.headers.get("content-length");
    if (requireContentLength && header === null)
      throw new Error(`${label} requires content length`);
    let declaredLength;
    if (header !== null) {
      const size = Number(header);
      if (
        !/^\d+$/u.test(header) ||
        !Number.isSafeInteger(size) ||
        size < 1 ||
        size > maxBytes ||
        (expectedBytes !== undefined && size !== expectedBytes)
      ) {
        throw new Error(`${label} returned an invalid bounded content length`);
      }
      declaredLength = size;
    }
    if (response.body === null) throw new Error(`${label} returned an empty body`);
    reader = response.body.getReader();
    const deadline = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} body read timed out`)), timeoutMs);
    });
    const limit = expectedBytes ?? declaredLength ?? maxBytes;
    const collect = async () => {
      const bytes = new Uint8Array(limit);
      let size = 0;
      let emptyChunks = 0;
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (!(value instanceof Uint8Array)) throw new Error(`${label} returned invalid body bytes`);
        if (value.byteLength === 0) {
          if (++emptyChunks > 1_024) throw new Error(`${label} exceeded its empty chunk bound`);
          continue;
        }
        emptyChunks = 0;
        if (value.byteLength > limit - size)
          throw new Error(`${label} exceeded its bounded body limit`);
        bytes.set(value, size);
        size += value.byteLength;
      }
      if (
        size === 0 ||
        (expectedBytes !== undefined && size !== expectedBytes) ||
        (declaredLength !== undefined && size !== declaredLength)
      ) {
        throw new Error(`${label} stream length differs`);
      }
      return bytes.subarray(0, size);
    };
    // One race for the entire collection avoids retaining a deadline reaction per chunk.
    return await Promise.race([collect(), deadline]);
  } catch (error) {
    try {
      const cancellation = reader === undefined ? response.body?.cancel() : reader.cancel();
      void Promise.resolve(cancellation).catch(() => undefined);
    } catch {
      /* Preserve the admission/read error. */
    }
    throw error;
  } finally {
    clearTimeout(timer);
    try {
      reader?.releaseLock();
    } catch {
      /* A timed-out read can still be pending. */
    }
  }
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
