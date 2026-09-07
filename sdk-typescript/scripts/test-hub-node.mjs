import { readFile, writeFile, chmod } from "node:fs/promises";
import { pathToFileURL } from "node:url";
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
const drives = await client.drives(vehicleId, { limit: 2 });
if (drives.kind !== "page") throw new Error("initial drives request was not a page");
await client.rotateDevice();
await client.vehicles();
const receipt = {
  runtime: `node ${process.version}`,
  hubId: discovery.value.hubId,
  apiVersions: discovery.value.apiVersions,
  capabilityCount: discovery.value.capabilities.length,
  vehicleCount: firstVehicles.value.vehicles.length,
  driveCount: drives.value.items.length,
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
