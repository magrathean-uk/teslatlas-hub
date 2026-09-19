import { chmod, readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { collectDrivePages } from "./drive-pagination.mjs";
import { verifyPackedSdk } from "./hub-acceptance-evidence.mjs";

const descriptorPath = required("TESLATLAS_HUB_HTTP_CONFIG");
const packedSdk = await verifyPackedSdk({
  packageRoot: required("TESLATLAS_HUB_SDK_PACKAGE_ROOT"),
  tarballPath: required("TESLATLAS_HUB_SDK_TARBALL"),
  expectedTarballSha256: required("TESLATLAS_HUB_SDK_TARBALL_SHA256"),
  entryKind: "node",
});
const descriptor = JSON.parse(await readFile(descriptorPath, "utf8"));
const invitation = JSON.parse(await readFile(descriptor.invitation_path, "utf8"));
const driveLimit = integerOption(
  descriptor.drive_limit ??
    descriptor.driveLimit ??
    process.env.TESLATLAS_HUB_DRIVE_PAGE_LIMIT ??
    2,
  "drive page limit",
);
const expectedDriveCount = optionalInteger(
  descriptor.expected_drive_count ??
    descriptor.expectedDriveCount ??
    process.env.TESLATLAS_HUB_EXPECTED_DRIVE_COUNT,
  "expected drive count",
);
const { createHubClient } = await import(pathToFileURL(packedSdk.entry).href);
let credential;
const credentials = {
  load: () => credential,
  save: (next) => {
    credential = next;
  },
  clear: () => {
    credential = undefined;
  },
};
const client = createHubClient({
  endpoint: descriptor.endpoint,
  expectedHubId: descriptor.hub_id,
  credentials,
});
const discovery = await client.discover();
await client.health();
await client.readiness();
await client.claimPairing(invitation, "Packed TypeScript SDK Node acceptance");
const firstVehicles = await client.vehicles();
if (firstVehicles.value.vehicles.length === 0) throw new Error("fixture returned no vehicles");
const vehicleId = firstVehicles.value.vehicles[0].vehicleId;
await client.current(vehicleId);
const drivePages = await collectDrivePages(client, vehicleId, { limit: driveLimit });
const firstDrivePage = drivePages[0];
const driveCount = drivePages.reduce((count, page) => count + page.value.items.length, 0);
if (expectedDriveCount !== undefined && driveCount !== expectedDriveCount) {
  throw new Error(`expected ${expectedDriveCount} drives, received ${driveCount}`);
}
const refresh = await client.drives(vehicleId, {
  limit: driveLimit,
  ifNoneMatch: firstDrivePage.metadata.etag,
});
if (refresh.kind !== "notModified") throw new Error("conditional drives refresh was not 304");
let post304CursorPage;
if (firstDrivePage.value.nextCursor !== null) {
  post304CursorPage = await client.drives(vehicleId, {
    limit: driveLimit,
    cursor: firstDrivePage.value.nextCursor,
  });
  if (post304CursorPage.kind !== "page") {
    throw new Error("cursor continuation after 304 was not a page");
  }
}
await client.rotateDevice();
await client.vehicles();
const receipt = {
  runtime: `node ${process.version}`,
  hubId: discovery.value.hubId,
  apiVersions: discovery.value.apiVersions,
  capabilityCount: discovery.value.capabilities.length,
  vehicleCount: firstVehicles.value.vehicles.length,
  driveCount,
  drivePageCount: drivePages.length,
  drivePageLimit: driveLimit,
  driveEtag304: true,
  cursorContinuationAfter304: post304CursorPage !== undefined,
  invitationClaim: "passed",
  credentialRotation: "passed",
  defaultFetch: true,
  packedSdk: packedSdk.witness,
};
if (process.env.TESLATLAS_HUB_CREDENTIAL_OUT) {
  await writeFile(process.env.TESLATLAS_HUB_CREDENTIAL_OUT, `${JSON.stringify(credential)}\n`, {
    mode: 0o600,
    flag: "wx",
  });
  await chmod(process.env.TESLATLAS_HUB_CREDENTIAL_OUT, 0o600);
}
if (process.env.TESLATLAS_HUB_RECEIPT) {
  await writeFile(process.env.TESLATLAS_HUB_RECEIPT, `${JSON.stringify(receipt, null, 2)}\n`, {
    mode: 0o600,
    flag: "wx",
  });
}
client.dispose();
console.log(JSON.stringify(receipt));

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function integerOption(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 500) {
    throw new Error(`${label} must be an integer between 1 and 500`);
  }
  return parsed;
}

function optionalInteger(value, label) {
  if (value === undefined) return undefined;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    throw new Error(`${label} must be a non-negative integer`);
  }
  return parsed;
}
