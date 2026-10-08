import { readFile, stat } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import {
  canonicalizeSortedJson,
  collectRedactedSemanticSnapshot,
  createReadOnlyCredentialStore,
  hashSortedJson,
  SEMANTIC_SNAPSHOT_CANONICALIZATION,
} from "./redacted-semantic-snapshot.mjs";

const descriptor = JSON.parse(await readFile(required("TESLATLAS_HUB_HTTP_CONFIG"), "utf8"));
const credential = JSON.parse(await readFile(required("TESLATLAS_HUB_CREDENTIAL_FILE"), "utf8"));
validateCredential(credential);
const semanticProfile = await readOptionalPrivateJson(
  process.env.TESLATLAS_HUB_SEMANTIC_PROFILE_FILE,
  "semantic profile",
);
const nodeEntry = join(required("TESLATLAS_HUB_SDK_PACKAGE_ROOT"), "dist/node.js");
const { createHubClient } = await import(pathToFileURL(nodeEntry).href);
const client = createHubClient({
  endpoint: descriptor.endpoint,
  expectedHubId: descriptor.hub_id,
  credentials: createReadOnlyCredentialStore(credential),
});

try {
  const semanticSnapshot = await collectRedactedSemanticSnapshot(client, semanticProfile);
  const semanticSnapshotSha256 = await hashSortedJson(semanticSnapshot);
  process.stdout.write(
    `${JSON.stringify({
      canonicalization: SEMANTIC_SNAPSHOT_CANONICALIZATION,
      semanticSnapshot,
      semanticSnapshotSha256,
      canonicalByteLength: new TextEncoder().encode(canonicalizeSortedJson(semanticSnapshot))
        .byteLength,
    })}\n`,
  );
} finally {
  client.dispose();
}

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
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

async function readOptionalPrivateJson(path, label) {
  if (path === undefined || path === "") return undefined;
  const metadata = await stat(path, { bigint: true });
  if (!metadata.isFile()) throw new Error(`${label} must be a file`);
  if (metadata.uid !== BigInt(process.getuid())) {
    throw new Error(`${label} must be owned by the current user`);
  }
  if ((metadata.mode & 0o077n) !== 0n) throw new Error(`${label} must be owner-only`);
  return JSON.parse(await readFile(path, "utf8"));
}
