import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, readFile, writeFile } from "node:fs/promises";
import http from "node:http";
import { join } from "node:path";
import { firefox } from "playwright";
import { observeFirefoxCorsReads } from "./firefox-cors-evidence.mjs";
import {
  FIREFOX_NSS_CERTUTIL,
  requireOwnerOnlyOutputPath,
  requireOwnerOnlyPath,
  withFirefoxNssProfile,
} from "./firefox-nss-profile.mjs";

const descriptorPath = required("TESLATLAS_HUB_HTTP_CONFIG");
const credentialPath = required("TESLATLAS_HUB_CREDENTIAL_FILE");
await requireOwnerOnlyPath(descriptorPath, "Hub descriptor", "file");
await requireOwnerOnlyPath(credentialPath, "Hub credential", "file");
const descriptor = JSON.parse(await readFile(descriptorPath, "utf8"));
const credential = JSON.parse(await readFile(credentialPath, "utf8"));
const semanticProfilePath = process.env.TESLATLAS_HUB_SEMANTIC_PROFILE_FILE;
if (semanticProfilePath)
  await requireOwnerOnlyPath(semanticProfilePath, "semantic profile", "file");
const semanticProfile = semanticProfilePath
  ? JSON.parse(await readFile(semanticProfilePath, "utf8"))
  : undefined;
const packageRoot = required("TESLATLAS_HUB_SDK_PACKAGE_ROOT");
const certificatePath = descriptor.certificate_path;
const origin = new URL(required("TESLATLAS_HUB_BROWSER_ORIGIN"));
const receiptPath = required("TESLATLAS_HUB_BROWSER_RECEIPT");
await requireOwnerOnlyOutputPath(receiptPath, "browser receipt");
if (origin.href !== "http://127.0.0.1:4174/") throw new Error("unexpected browser origin");
validateCredential(credential);

const browserEntry = join(packageRoot, "dist/browser.js");
const pairingModule = await readFile(new URL("./browser-pairing-preflight.mjs", import.meta.url));
const driveModule = await readFile(new URL("./drive-pagination.mjs", import.meta.url));
const semanticSnapshotModule = await readFile(
  new URL("./redacted-semantic-snapshot.mjs", import.meta.url),
);
const config = {
  credential,
  endpoint: descriptor.endpoint,
  hubId: descriptor.hub_id,
  semanticProfile,
};
const server = createServer({
  browserEntry,
  pairingModule,
  driveModule,
  semanticSnapshotModule,
  config,
});
let route;
try {
  await listen(server, Number(origin.port));
  route = await withFirefoxNssProfile(
    {
      certificatePath,
      certutilPath: FIREFOX_NSS_CERTUTIL,
      firefox,
    },
    (context) => observeFirefoxCorsReads(context, origin, descriptor.endpoint),
  );
} finally {
  await closeServer(server);
}
if (route === undefined) throw new Error("Firefox NSS route produced no result");
const positive = route.value;

const receipt = {
  schemaVersion: 1,
  result: "MAC3_REAL_FIREFOX_NSS_CORS_READS_PASS",
  hubId: positive.hubId,
  runtime: positive.runtime,
  origin: origin.origin,
  vehicleCount: positive.vehicleCount,
  driveCount: positive.driveCount,
  drivePageCount: positive.drivePageCount,
  etag304: positive.etag304,
  cursorContinuationAfter304: positive.cursorContinuationAfter304,
  semanticSnapshot: positive.semanticSnapshot,
  semanticSnapshotSha256: positive.semanticSnapshotSha256,
  semanticSnapshotCanonicalization: positive.semanticSnapshotCanonicalization,
  browserPairing: positive.browserPairing,
  defaultFetch: positive.defaultFetch,
  cors: positive.cors,
  package: {
    entry: "dist/browser.js",
    entrySha256: createHash("sha256")
      .update(await readFile(browserEntry))
      .digest("hex"),
  },
  trust: {
    ...route.evidence,
    normalCertificateValidation: true,
  },
  cleanup: { browsers: "closed", helperServer: "closed", profiles: "removed" },
};
await writeFile(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, {
  mode: 0o600,
  flag: "wx",
});
await chmod(receiptPath, 0o600);
process.stdout.write(`${JSON.stringify(receipt)}\n`);

function createServer({
  browserEntry,
  pairingModule,
  driveModule,
  semanticSnapshotModule,
  config,
}) {
  return http.createServer((request, response) => {
    if (request.url === "/") {
      response.setHeader("content-type", "text/html; charset=utf-8");
      return response.end(html());
    }
    if (request.url === "/config.json") {
      response.setHeader("content-type", "application/json");
      return response.end(JSON.stringify(config));
    }
    if (request.url === "/sdk.js") {
      response.setHeader("content-type", "text/javascript");
      return createReadStream(browserEntry).pipe(response);
    }
    if (request.url === "/browser-pairing-preflight.mjs") {
      response.setHeader("content-type", "text/javascript");
      return response.end(pairingModule);
    }
    if (request.url === "/drive-pagination.mjs") {
      response.setHeader("content-type", "text/javascript");
      return response.end(driveModule);
    }
    if (request.url === "/redacted-semantic-snapshot.mjs") {
      response.setHeader("content-type", "text/javascript");
      return response.end(semanticSnapshotModule);
    }
    response.writeHead(404).end();
  });
}

function html() {
  return `<!doctype html><meta charset="utf-8"><script type="module">
import { createHubClient } from "/sdk.js";
import { createProvisionedBrowserClientAndCheckPairing } from "/browser-pairing-preflight.mjs";
import { collectDrivePages } from "/drive-pagination.mjs";
import { collectRedactedSemanticSnapshot, hashSortedJson, selectSemanticSnapshotPrimaryVehicle, SEMANTIC_SNAPSHOT_CANONICALIZATION } from "/redacted-semantic-snapshot.mjs";
const config = await (await fetch("/config.json")).json();
let credential = config.credential;
const credentials = {load:()=>credential,save:(value)=>{credential=value;},clear:()=>{credential=undefined;}};
try {
 const {client,pairing:browserPairing,readsUseDefaultFetch} = await createProvisionedBrowserClientAndCheckPairing({createHubClient,endpoint:config.endpoint,hubId:config.hubId,credentials});
 const discovery = await client.discover(); await client.readiness();
 const vehicles = await client.vehicles();
 const vehicleId = config.semanticProfile
   ? selectSemanticSnapshotPrimaryVehicle(vehicles.value.vehicles,config.semanticProfile)
   : vehicles.value.vehicles[0]?.vehicleId;
 if (!vehicleId) throw new Error("Hub returned no browser-visible vehicle");
 await client.current(vehicleId);
 const pages = await collectDrivePages(client,vehicleId,{limit:2});
 const first = pages[0];
 const refresh = await client.drives(vehicleId,{limit:2,ifNoneMatch:first.metadata.etag});
 if (refresh.kind !== "notModified") throw new Error("browser conditional read was not 304");
 let continuation=false;
 if (first.value.nextCursor !== null) {
   const next = await client.drives(vehicleId,{limit:2,cursor:first.value.nextCursor});
   if (next.kind !== "page") throw new Error("browser cursor continuation failed");
   continuation=true;
 }
 const semanticSnapshot = config.semanticProfile
   ? await collectRedactedSemanticSnapshot(client,config.semanticProfile)
   : null;
 const semanticSnapshotSha256 = semanticSnapshot
   ? await hashSortedJson(semanticSnapshot)
   : null;
 client.dispose();
 window.__teslatlasResult={ok:true,pageOrigin:location.origin,hubId:discovery.value.hubId,vehicleCount:vehicles.value.vehicles.length,driveCount:pages.reduce((n,p)=>n+p.value.items.length,0),drivePageCount:pages.length,etag304:true,cursorContinuationAfter304:continuation,semanticSnapshot,semanticSnapshotSha256,semanticSnapshotCanonicalization:SEMANTIC_SNAPSHOT_CANONICALIZATION,browserPairing,defaultFetch:readsUseDefaultFetch};
} catch (error) { window.__teslatlasResult={ok:false,error:String(error?.stack??error)}; }
</script>`;
}

async function listen(target, port) {
  await new Promise((resolve, reject) => {
    target.once("error", reject).listen(port, "127.0.0.1", resolve);
  });
}

async function closeServer(target) {
  if (!target.listening) return;
  await new Promise((resolve, reject) =>
    target.close((error) => (error ? reject(error) : resolve())),
  );
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
