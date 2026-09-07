import { createReadStream } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import http from "node:http";
import { chromium } from "playwright";
import { verifyBrowserTrustWitness, verifyPackedSdk } from "./hub-acceptance-evidence.mjs";

const descriptor = JSON.parse(await readFile(required("TESLATLAS_HUB_HTTP_CONFIG"), "utf8"));
const credential = JSON.parse(await readFile(required("TESLATLAS_HUB_CREDENTIAL_FILE"), "utf8"));
const packedSdk = await verifyPackedSdk({
  packageRoot: required("TESLATLAS_HUB_SDK_PACKAGE_ROOT"),
  tarballPath: required("TESLATLAS_HUB_SDK_TARBALL"),
  expectedTarballSha256: required("TESLATLAS_HUB_SDK_TARBALL_SHA256"),
  entryKind: "browser",
});
const sdk = packedSdk.entry;
const origin = new URL(required("TESLATLAS_HUB_BROWSER_ORIGIN"));
if (origin.hostname !== "localhost" || origin.protocol !== "http:" || origin.pathname !== "/")
  throw new Error("browser origin must be http://localhost:PORT");
const endpoint = new URL(descriptor.endpoint);
endpoint.hostname = "localhost";
const config = {
  endpoint: endpoint.href.replace(/\/$/u, ""),
  hubId: descriptor.hub_id,
  credential,
};
const listenPort = Number(process.env.TESLATLAS_HUB_BROWSER_LISTEN_PORT ?? origin.port);
const html = `<!doctype html><meta charset="utf-8"><script type="module">
import { createHubClient } from "/sdk.js";
const config = await (await fetch("/config.json")).json();
let credential = config.credential;
const credentials = { load: () => credential, save: value => { credential = value; }, clear: () => { credential = undefined; } };
try {
 const client = createHubClient({endpoint: config.endpoint, expectedHubId: config.hubId, credentials});
 const discovery = await client.discover(); await client.readiness();
 const vehicles = await client.vehicles(); const vehicleId = vehicles.value.vehicles[0].vehicleId;
 await client.current(vehicleId);
 const first = await client.drives(vehicleId, {limit: 2});
 if (first.kind !== "page" || first.value.nextCursor === null) throw new Error("first drives page/cursor missing");
 const second = await client.drives(vehicleId, {limit: 2, cursor:first.value.nextCursor});
 if (second.kind !== "page" || second.value.nextCursor === null) throw new Error("second drives page/cursor missing");
 const third = await client.drives(vehicleId, {limit: 2, cursor:second.value.nextCursor});
 if (third.kind !== "page" || third.value.nextCursor !== null) throw new Error("terminal drives page invalid");
 const refresh = await client.drives(vehicleId, {limit:2, ifNoneMatch:first.metadata.etag});
 if (refresh.kind !== "notModified") throw new Error("conditional drives refresh was not 304");
 const after304 = await client.drives(vehicleId, {limit:2, cursor:first.value.nextCursor});
 if (after304.kind !== "page") throw new Error("cursor continuation after 304 failed");
 await client.rotateDevice(); await client.vehicles(); client.dispose();
 window.__teslatlasResult = {ok:true, hubId:discovery.value.hubId, vehicleCount:vehicles.value.vehicles.length, drivePageIds:[first,second,third].map(page => page.value.items.map(item => String(item.id))), terminalCursor:third.value.nextCursor === null, notModified:refresh.kind === "notModified", post304CursorIds:after304.value.items.map(item => String(item.id))};
} catch (error) { window.__teslatlasResult = {ok:false, error:String(error?.stack ?? error)}; }
</script>`;
const server = http.createServer((request, response) => {
  if (request.url === "/") return response.end(html);
  if (request.url === "/config.json") {
    response.setHeader("content-type", "application/json");
    return response.end(JSON.stringify(config));
  }
  if (request.url === "/sdk.js") {
    response.setHeader("content-type", "text/javascript");
    return createReadStream(sdk).pipe(response);
  }
  response.writeHead(404).end();
});
await new Promise((resolve, reject) =>
  server.listen(listenPort, "127.0.0.1", resolve).once("error", reject),
);
try {
  const browser = await chromium.connectOverCDP(required("TESLATLAS_BROWSER_CDP_URL"));
  const untrustedBrowser = await chromium.connectOverCDP(
    required("TESLATLAS_BROWSER_UNTRUSTED_CDP_URL"),
  );
  const trustedCdp = await browser.newBrowserCDPSession();
  const untrustedCdp = await untrustedBrowser.newBrowserCDPSession();
  const trustedArguments = (await trustedCdp.send("Browser.getBrowserCommandLine")).arguments;
  const untrustedArguments = (await untrustedCdp.send("Browser.getBrowserCommandLine")).arguments;
  const trust = await verifyBrowserTrustWitness({
    witnessPath: required("TESLATLAS_BROWSER_LAUNCH_WITNESS"),
    certificatePath: descriptor.certificate_path,
    trustedArguments,
    untrustedArguments,
  });
  const untrustedContext = untrustedBrowser.contexts()[0] ?? (await untrustedBrowser.newContext());
  const untrustedPage = await untrustedContext.newPage();
  let untrustedTlsError;
  try {
    await untrustedPage.goto(`${config.endpoint}/healthz`, { waitUntil: "load", timeout: 10_000 });
  } catch (error) {
    untrustedTlsError = String(error);
  }
  if (!untrustedTlsError?.includes("ERR_CERT_AUTHORITY_INVALID")) {
    throw new Error("untrusted browser control did not fail with ERR_CERT_AUTHORITY_INVALID");
  }
  await untrustedPage.close();
  const context = browser.contexts()[0] ?? (await browser.newContext());
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  let preflightCount = 0;
  cdp.on("Network.requestWillBeSent", (event) => {
    if (event.request.method === "OPTIONS" && event.request.url.startsWith(config.endpoint))
      preflightCount += 1;
  });
  await cdp.send("Network.enable");
  await page.goto(origin.href, { waitUntil: "load" });
  await page.waitForFunction(() => window.__teslatlasResult !== undefined, null, {
    timeout: 30_000,
  });
  const result = await page.evaluate(() => window.__teslatlasResult);
  if (!result.ok) throw new Error(result.error);
  if (preflightCount === 0) throw new Error("no Hub CORS preflight was observed");
  const receipt = {
    runtime: await browser.version(),
    origin: origin.origin,
    ...result,
    corsPreflightCount: preflightCount,
    normalCertificateValidation:
      trust.certificateSha256.length === 64 &&
      untrustedTlsError.includes("ERR_CERT_AUTHORITY_INVALID"),
    certificateSha256: trust.certificateSha256,
    trustedNssDatabase: trust.trustedNssDatabase,
    untrustedControlError: "ERR_CERT_AUTHORITY_INVALID",
    defaultFetch: true,
    packedSdk: packedSdk.witness,
  };
  if (process.env.TESLATLAS_HUB_RECEIPT)
    await writeFile(process.env.TESLATLAS_HUB_RECEIPT, `${JSON.stringify(receipt, null, 2)}\n`, {
      mode: 0o600,
      flag: "wx",
    });
  console.log(JSON.stringify(receipt));
  await page.close();
  await browser.close();
  await untrustedBrowser.close();
} finally {
  await new Promise((resolve) => server.close(resolve));
}

function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}
