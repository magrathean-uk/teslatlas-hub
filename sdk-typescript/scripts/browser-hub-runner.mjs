import { execFile } from "node:child_process";
import { createReadStream } from "node:fs";
import { lstat, readFile, realpath, writeFile } from "node:fs/promises";
import http from "node:http";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { promisify } from "node:util";
import { chromium } from "playwright";
import {
  runSequentialMacTrustSequence,
  SEQUENTIAL_MACOS_TRUST_MODE,
} from "./browser-trust-sequence.mjs";
import {
  assertSafeBrowserArguments,
  certificateDigest,
  verifyBrowserTrustWitness,
  verifyPackedSdk,
} from "./hub-acceptance-evidence.mjs";

const execFileAsync = promisify(execFile);

export async function runBrowserHubAcceptance() {
  const resources = { browsers: new Set(), server: undefined };
  let outcome;
  let primaryError;

  try {
    const descriptor = JSON.parse(await readFile(required("TESLATLAS_HUB_HTTP_CONFIG"), "utf8"));
    const credential = JSON.parse(
      await readFile(required("TESLATLAS_HUB_CREDENTIAL_FILE"), "utf8"),
    );
    const drivePaginationModule = await readFile(
      new URL("./drive-pagination.mjs", import.meta.url),
      "utf8",
    );
    const packedSdk = await verifyPackedSdk({
      packageRoot: required("TESLATLAS_HUB_SDK_PACKAGE_ROOT"),
      tarballPath: required("TESLATLAS_HUB_SDK_TARBALL"),
      expectedTarballSha256: required("TESLATLAS_HUB_SDK_TARBALL_SHA256"),
      entryKind: "browser",
    });
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
    const origin = new URL(required("TESLATLAS_HUB_BROWSER_ORIGIN"));
    if (origin.hostname !== "localhost" || origin.protocol !== "http:" || origin.pathname !== "/")
      throw new Error("browser origin must be http://localhost:PORT");
    const endpoint = new URL(descriptor.endpoint);
    endpoint.hostname = "localhost";
    const config = {
      endpoint: endpoint.href.replace(/\/$/u, ""),
      hubId: descriptor.hub_id,
      credential,
      driveLimit,
      expectedDriveCount: expectedDriveCount ?? null,
    };
    const sdk = packedSdk.entry;
    const html = browserRouteHtml();
    resources.server = createHelperServer({ html, config, sdk, drivePaginationModule });
    const listenPort = Number(process.env.TESLATLAS_HUB_BROWSER_LISTEN_PORT ?? origin.port);
    await listen(resources.server, listenPort);

    const trustMode = process.env.TESLATLAS_BROWSER_TRUST_MODE;
    if (trustMode === SEQUENTIAL_MACOS_TRUST_MODE) {
      outcome = await runSequentialRoute({
        descriptor,
        config,
        origin,
        packedSdk,
        resources,
      });
    } else if (trustMode === undefined || trustMode === "legacy-simultaneous") {
      outcome = await runLegacyRoute({
        descriptor,
        config,
        origin,
        packedSdk,
        resources,
      });
    } else {
      throw new Error(`unsupported browser trust mode: ${trustMode}`);
    }
  } catch (error) {
    primaryError = error;
  }

  const cleanupErrors = await cleanupResources(resources);
  if (primaryError !== undefined) {
    attachCleanupErrors(primaryError, cleanupErrors);
    throw primaryError;
  }
  if (cleanupErrors.length > 0) {
    throw new AggregateError(cleanupErrors, "browser acceptance cleanup failed");
  }
  if (outcome === undefined) throw new Error("browser acceptance produced no outcome");

  const receipt = {
    ...outcome,
    cleanup: {
      browsers: "closed",
      helperServer: "closed",
      trust:
        outcome.trustMode === SEQUENTIAL_MACOS_TRUST_MODE
          ? "removed-and-verified"
          : "legacy-witness-verified",
    },
  };
  if (process.env.TESLATLAS_HUB_RECEIPT)
    await writeFile(process.env.TESLATLAS_HUB_RECEIPT, `${JSON.stringify(receipt, null, 2)}\n`, {
      mode: 0o600,
      flag: "wx",
    });
  console.log(JSON.stringify(receipt));
  return receipt;
}

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

async function listen(server, port) {
  await new Promise((resolve, reject) => {
    server.listen(port, "127.0.0.1", resolve).once("error", reject);
  });
}

function trackBrowser(resources, browser) {
  resources.browsers.add(browser);
  return browser;
}

async function closeTrackedBrowser(resources, browser) {
  await browser.close();
  resources.browsers.delete(browser);
}

async function cleanupResources(resources) {
  const errors = [];
  for (const browser of resources.browsers) {
    try {
      await closeTrackedBrowser(resources, browser);
    } catch (error) {
      errors.push(error);
    }
  }
  if (resources.server?.listening) {
    try {
      await new Promise((resolve, reject) => {
        resources.server.close((error) => (error ? reject(error) : resolve()));
      });
    } catch (error) {
      errors.push(error);
    }
  }
  return errors;
}

function attachCleanupErrors(error, cleanupErrors) {
  if (cleanupErrors.length === 0) return;
  const cleanupError = new AggregateError(cleanupErrors, "browser acceptance cleanup failed");
  if (error instanceof Error) error.cleanupError = cleanupError;
}

function createHelperServer({ html, config, sdk, drivePaginationModule }) {
  return http.createServer((request, response) => {
    if (request.url === "/") return response.end(html);
    if (request.url === "/config.json") {
      response.setHeader("content-type", "application/json");
      return response.end(JSON.stringify(config));
    }
    if (request.url === "/sdk.js") {
      response.setHeader("content-type", "text/javascript");
      return createReadStream(sdk).pipe(response);
    }
    if (request.url === "/drive-pagination.mjs") {
      response.setHeader("content-type", "text/javascript");
      return response.end(drivePaginationModule);
    }
    response.writeHead(404).end();
  });
}

function browserRouteHtml() {
  return `<!doctype html><meta charset="utf-8"><script type="module">
import { createHubClient } from "/sdk.js";
import { collectDrivePages } from "/drive-pagination.mjs";
const config = await (await fetch("/config.json")).json();
let credential = config.credential;
const credentials = { load: () => credential, save: value => { credential = value; }, clear: () => { credential = undefined; } };
try {
 const client = createHubClient({endpoint: config.endpoint, expectedHubId: config.hubId, credentials});
 const discovery = await client.discover(); await client.readiness();
 const vehicles = await client.vehicles();
 if (vehicles.value.vehicles.length === 0) throw new Error("fixture returned no vehicles");
 const vehicleId = vehicles.value.vehicles[0].vehicleId;
 await client.current(vehicleId);
 const pages = await collectDrivePages(client, vehicleId, {limit: config.driveLimit});
 const first = pages[0];
 const driveCount = pages.reduce((count, page) => count + page.value.items.length, 0);
 if (config.expectedDriveCount !== null && driveCount !== config.expectedDriveCount) throw new Error("expected " + config.expectedDriveCount + " drives, received " + driveCount);
 const refresh = await client.drives(vehicleId, {limit:config.driveLimit, ifNoneMatch:first.metadata.etag});
 if (refresh.kind !== "notModified") throw new Error("conditional drives refresh was not 304");
 let after304;
 if (first.value.nextCursor !== null) {
  after304 = await client.drives(vehicleId, {limit:config.driveLimit, cursor:first.value.nextCursor});
  if (after304.kind !== "page") throw new Error("cursor continuation after 304 failed");
 }
 await client.rotateDevice(); await client.vehicles(); client.dispose();
 window.__teslatlasResult = {ok:true, hubId:discovery.value.hubId, vehicleCount:vehicles.value.vehicles.length, driveCount, drivePageCount:pages.length, drivePageLimit:config.driveLimit, expectedDriveCount:config.expectedDriveCount, drivePageIds:pages.map(page => page.value.items.map(item => String(item.id))), terminalCursor:pages.at(-1).value.nextCursor === null, notModified:refresh.kind === "notModified", post304CursorIds:after304?.value.items.map(item => String(item.id)) ?? [], defaultFetch:true, pageOrigin:location.origin};
} catch (error) { window.__teslatlasResult = {ok:false, error:String(error?.stack ?? error)}; }
</script>`;
}

async function runLegacyRoute({ descriptor, config, origin, packedSdk, resources }) {
  const browser = trackBrowser(
    resources,
    await chromium.connectOverCDP(required("TESLATLAS_BROWSER_CDP_URL")),
  );
  const untrustedBrowser = trackBrowser(
    resources,
    await chromium.connectOverCDP(required("TESLATLAS_BROWSER_UNTRUSTED_CDP_URL")),
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
  const untrustedObservation = await observeUntrustedBrowser(
    untrustedBrowser,
    config.endpoint,
    trust.certificateSha256,
    trust.untrustedTrustStore,
  );
  if (!untrustedObservation.error?.includes("ERR_CERT_AUTHORITY_INVALID")) {
    throw new Error("untrusted browser control did not fail with ERR_CERT_AUTHORITY_INVALID");
  }
  const route = await runTrustedRoute({ browser, config, origin });
  return {
    ...publicRouteReceipt(route),
    trustMode: trust.trustMode,
    trustOrder: ["untrusted_observed", "trusted_route_observed"],
    normalCertificateValidation: true,
    certificateSha256: trust.certificateSha256,
    trustedTrustStore: trust.trustedTrustStore,
    untrustedTrustStore: trust.untrustedTrustStore,
    certificateExportPath: trust.certificateExportPath,
    untrustedControlError: "ERR_CERT_AUTHORITY_INVALID",
    packedSdk: packedSdk.witness,
  };
}

async function runSequentialRoute({ descriptor, config, origin, packedSdk, resources }) {
  const certificatePath = descriptor.certificate_path;
  const certificateSha256 = certificateDigest(await readFile(certificatePath));
  const witnessPath = required("TESLATLAS_BROWSER_LAUNCH_WITNESS");
  const untrustedTrustStorePath = required("TESLATLAS_BROWSER_UNTRUSTED_TRUST_STORE_PATH");
  const expectedTrustedCdpUrl = required("TESLATLAS_BROWSER_CDP_URL");
  const expectedCertificateExportPath = required(
    "TESLATLAS_BROWSER_EXPECTED_CERTIFICATE_EXPORT_PATH",
  );
  await requireFreshCanonicalPath(expectedCertificateExportPath);
  await assertAbsent(expectedCertificateExportPath);
  const importCommand = trustCommand("TESLATLAS_BROWSER_TRUST_IMPORT_COMMAND");
  const trustedBrowserCleanupCommand = trustCommand(
    "TESLATLAS_BROWSER_TRUST_BROWSER_CLEANUP_COMMAND",
  );
  const removeCommand = trustCommand("TESLATLAS_BROWSER_TRUST_REMOVE_COMMAND");
  const verifyRemovalCommand = trustCommand("TESLATLAS_BROWSER_TRUST_VERIFY_REMOVAL_COMMAND");

  const sequence = await runSequentialMacTrustSequence({
    endpoint: config.endpoint,
    certificatePath,
    certificateSha256,
    openUntrusted: async () =>
      trackBrowser(
        resources,
        await chromium.connectOverCDP(required("TESLATLAS_BROWSER_UNTRUSTED_CDP_URL")),
      ),
    observeUntrusted: async (browser, context) =>
      observeUntrustedBrowser(
        browser,
        context.endpoint,
        context.certificateSha256,
        untrustedTrustStorePath,
      ),
    closeUntrusted: (browser) => closeTrackedBrowser(resources, browser),
    importCertificate: async (context) => {
      const imported = await runTrustHook(
        importCommand,
        browserTrustHookPayload("import", {
          endpoint: context.endpoint,
          certificatePath: context.certificatePath,
          certificateSha256: context.certificateSha256,
          trustedCdpUrl: expectedTrustedCdpUrl,
          expectedCertificateExportPath,
        }),
      );
      if (imported.certificateExportPath !== expectedCertificateExportPath) {
        throw new Error("certificate import returned an unexpected export path");
      }
      if (imported.trustedCdpUrl !== expectedTrustedCdpUrl) {
        throw new Error("certificate import returned an unexpected trusted CDP URL");
      }
      return imported;
    },
    openTrusted: async (imported) =>
      trackBrowser(resources, await chromium.connectOverCDP(imported.trustedCdpUrl)),
    observeTrusted: async (browser, context) => {
      const route = await runTrustedRoute({ browser, config, origin });
      return {
        observed: true,
        endpoint: context.endpoint,
        certificateSha256: context.certificateSha256,
        arguments: route.browserArguments,
        cdpResult: route.cdpResult,
        route,
      };
    },
    verifyWitness: async ({
      endpoint,
      certificatePath: phaseCertificatePath,
      certificateSha256: phaseCertificateSha256,
      imported,
      untrustedObservation,
      trustedObservation,
    }) => {
      const witness = {
        schemaVersion: 2,
        mode: SEQUENTIAL_MACOS_TRUST_MODE,
        certificateSha256: phaseCertificateSha256,
        phaseOrder: [
          "untrusted_started_without_ca",
          "untrusted_healthz_rejected_ERR_CERT_AUTHORITY_INVALID",
          "untrusted_closed",
          "fresh_ca_imported",
          "trusted_started_with_ca",
          "trusted_route_observed",
        ],
        untrustedBeforeImport: {
          endpoint,
          arguments: untrustedObservation.arguments,
          error: untrustedObservation.error,
          trustStorePath: untrustedTrustStorePath,
          cdpResult: untrustedObservation.cdpResult,
        },
        trustedAfterImport: {
          endpoint,
          arguments: trustedObservation.arguments,
          certificateExportPath: imported.certificateExportPath,
          trustStorePath: imported.trustStorePath,
          cdpResult: trustedObservation.cdpResult,
        },
      };
      await writeFile(witnessPath, `${JSON.stringify(witness, null, 2)}\n`, {
        mode: 0o600,
        flag: "wx",
      });
      return verifyBrowserTrustWitness({
        mode: SEQUENTIAL_MACOS_TRUST_MODE,
        witnessPath,
        certificatePath: phaseCertificatePath,
        expectedCertificateExportPath,
        trustedArguments: trustedObservation.arguments,
        untrustedArguments: untrustedObservation.arguments,
      });
    },
    closeTrusted: (browser) => closeTrackedBrowser(resources, browser),
    cleanupTrusted: async (context) => {
      const cleaned = await runTrustHook(
        trustedBrowserCleanupCommand,
        browserTrustHookPayload("trusted-browser-cleanup", {
          endpoint: context.endpoint,
          certificatePath: context.certificatePath,
          certificateSha256: context.certificateSha256,
          fingerprint: context.fingerprint,
          trustedCdpUrl: expectedTrustedCdpUrl,
          expectedCertificateExportPath,
          witnessPath,
        }),
      );
      if (
        cleaned.endpoint !== context.endpoint ||
        cleaned.certificateSha256 !== context.certificateSha256 ||
        cleaned.trustedCdpUrl !== expectedTrustedCdpUrl
      ) {
        throw new Error("trusted browser cleanup returned an unexpected fresh binding");
      }
      return cleaned;
    },
    removeCertificate: (context) =>
      runTrustHook(
        removeCommand,
        browserTrustHookPayload("remove", {
          endpoint: context.endpoint,
          certificatePath: context.certificatePath,
          certificateSha256: context.certificateSha256,
          fingerprint: context.fingerprint,
        }),
      ),
    verifyCertificateRemoved: (context) =>
      runTrustHook(
        verifyRemovalCommand,
        browserTrustHookPayload("verify-removal", {
          endpoint: context.endpoint,
          certificatePath: context.certificatePath,
          certificateSha256: context.certificateSha256,
          fingerprint: context.fingerprint,
        }),
      ),
  });

  const route = sequence.trustedObservation.route;
  if (route === undefined) throw new Error("trusted route receipt was not observed");
  return {
    ...publicRouteReceipt(route),
    trustMode: sequence.mode,
    trustOrder: sequence.trustOrder,
    normalCertificateValidation: true,
    certificateSha256: sequence.certificateSha256,
    trustedTrustStore: sequence.imported.trustStorePath,
    untrustedTrustStore: untrustedTrustStorePath,
    certificateExportPath: sequence.imported.certificateExportPath,
    untrustedControlError: "ERR_CERT_AUTHORITY_INVALID",
    packedSdk: packedSdk.witness,
  };
}

export function browserTrustHookPayload(operation, payload) {
  return { operation, ...payload };
}

function publicRouteReceipt({
  cdpResult: _cdpResult,
  browserArguments: _browserArguments,
  ...receipt
}) {
  return receipt;
}

async function runTrustedRoute({ browser, config, origin }) {
  const browserCdp = await browser.newBrowserCDPSession();
  const browserArguments = (await browserCdp.send("Browser.getBrowserCommandLine")).arguments;
  assertSafeBrowserArguments(browserArguments);
  const context = browser.contexts()[0] ?? (await browser.newContext());
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  const optionRequests = new Set();
  let corsPreflightCount = 0;
  cdp.on("Network.requestWillBeSent", (event) => {
    if (event.request.method === "OPTIONS" && event.request.url.startsWith(config.endpoint)) {
      optionRequests.add(event.requestId);
    }
  });
  cdp.on("Network.responseReceived", (event) => {
    if (
      optionRequests.has(event.requestId) &&
      event.response.status >= 200 &&
      event.response.status < 300
    ) {
      optionRequests.delete(event.requestId);
      corsPreflightCount += 1;
    }
  });
  try {
    await cdp.send("Network.enable");
    await page.goto(origin.href, { waitUntil: "load" });
    await page.waitForFunction(() => window.__teslatlasResult !== undefined, null, {
      timeout: 30_000,
    });
    const cdpResult = await cdp.send("Runtime.evaluate", {
      expression: "window.__teslatlasResult",
      returnByValue: true,
    });
    const result = cdpResult.result?.value;
    if (!result?.ok) throw new Error(result?.error ?? "browser route failed");
    if (corsPreflightCount === 0) throw new Error("no successful Hub CORS preflight was observed");
    return {
      runtime: await browser.version(),
      origin: origin.origin,
      pageOrigin: result.pageOrigin,
      ...result,
      corsPreflightCount,
      cdpResult,
      browserArguments,
    };
  } finally {
    await page.close();
  }
}

export async function browserAndPageCdp(browser) {
  const browserCdp = await browser.newBrowserCDPSession();
  const context = browser.contexts()[0] ?? (await browser.newContext());
  const page = await context.newPage();
  const networkCdp = await context.newCDPSession(page);
  return { browserCdp, page, networkCdp };
}

async function observeUntrustedBrowser(browser, endpoint, certificateSha256, trustStorePath) {
  const { browserCdp, page, networkCdp: cdp } = await browserAndPageCdp(browser);
  const arguments_ = (await browserCdp.send("Browser.getBrowserCommandLine")).arguments;
  assertSafeBrowserArguments(arguments_);
  let cdpResult;
  cdp.on("Network.loadingFailed", (event) => {
    if (event.errorText?.includes("ERR_CERT_AUTHORITY_INVALID")) cdpResult = event;
  });
  await cdp.send("Network.enable");
  let error;
  try {
    await page.goto(`${endpoint}/healthz`, { waitUntil: "load", timeout: 10_000 });
  } catch (caught) {
    error = String(caught);
  } finally {
    await page.close();
  }
  return {
    observed: true,
    endpoint,
    certificateSha256,
    arguments: arguments_,
    error,
    cdpResult,
    trustStorePath,
  };
}

function trustCommand(name) {
  let command;
  try {
    command = JSON.parse(required(name));
  } catch (error) {
    throw new Error(`${name} must be a JSON argv array`, { cause: error });
  }
  if (
    !Array.isArray(command) ||
    command.length === 0 ||
    command.some((part) => typeof part !== "string" || part.length === 0)
  ) {
    throw new Error(`${name} must be a non-empty JSON argv array`);
  }
  return command;
}

async function runTrustHook(command, payload) {
  const result = await execFileAsync(command[0], [...command.slice(1), JSON.stringify(payload)], {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    shell: false,
  });
  const output = result.stdout.trim();
  if (output.length === 0) throw new Error("browser trust hook returned no JSON result");
  let parsed;
  try {
    parsed = JSON.parse(output);
  } catch (error) {
    throw new Error("browser trust hook returned invalid JSON", { cause: error });
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("browser trust hook result must be a JSON object");
  }
  return parsed;
}

async function requireFreshCanonicalPath(filePath) {
  if (!isAbsolute(filePath) || resolve(filePath) !== filePath) {
    throw new Error("expected certificate export path must be canonical and absolute");
  }
  const parent = await realpath(dirname(filePath));
  if (join(parent, basename(filePath)) !== filePath) {
    throw new Error("expected certificate export path must be canonical and absolute");
  }
}

async function assertAbsent(filePath) {
  try {
    await lstat(filePath);
  } catch (error) {
    if (error?.code === "ENOENT") return;
    throw error;
  }
  throw new Error("expected certificate export path must be absent before import");
}
