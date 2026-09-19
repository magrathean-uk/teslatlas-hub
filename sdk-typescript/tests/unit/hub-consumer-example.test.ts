import { createServer, type Server } from "node:http";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, type ChildProcess } from "node:child_process";
import { chromium } from "playwright";
import { describe, expect, it } from "vitest";

const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));
const exampleRoot = join(repositoryRoot, "examples/hub");
const fixtureRoot = new URL(
  "../../protocol/source/profiles/hub-http-v1/1.0.0/examples/",
  import.meta.url,
);
const hubA = "11111111-1111-4111-8111-111111111111";
const hubB = "22222222-2222-4222-8222-222222222222";
const vehicleId = hubA;
const accessToken = "0".repeat(64);
const secondAccessToken = "f".repeat(64);
const deviceId = "33333333-3333-4333-8333-333333333333";

describe("current Hub consumer example", () => {
  it("rejects a missing credential source before contacting a Hub", async () => {
    const result = await runNodeExample(["--endpoint", "http://127.0.0.1:1", "--hub-id", hubA]);

    expect(result.code).toBe(1);
    expect(result.stderr).toBe(
      "Teslatlas Hub Node consumer: exactly one of --invitation-file or --credential-file is required\n",
    );
  });

  it("runs a local fixture and reports an advertised missing drive capability as unavailable", async () => {
    const hub = await startHubFixture({
      capabilities: ["query.vehicles", "query.current"],
    });
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-hub-example-"));
    const credentialPath = join(directory, "credential.json");

    try {
      await writeCredential(credentialPath, hub.url, hub.hubId);
      const result = await runNodeExample([
        "--endpoint",
        hub.url,
        "--hub-id",
        hub.hubId,
        "--credential-file",
        credentialPath,
      ]);

      expect(result.code).toBe(0);
      expect(result.stderr).toBe("");
      expect(result.stdout).toBe(
        `Teslatlas Hub Node consumer: 1 vehicle(s), drives unavailable, Hub ${hub.hubId}\n`,
      );
      expect(
        hub.requests.some(
          (request) =>
            request.path === "/v1/vehicles" && request.authorization === `Bearer ${accessToken}`,
        ),
      ).toBe(true);
    } finally {
      await hub.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("uses a fresh timeout for each sequential Hub request", async () => {
    const hub = await startHubFixture({ delayMs: 70 });
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-hub-example-"));
    const credentialPath = join(directory, "credential.json");

    try {
      await writeCredential(credentialPath, hub.url, hub.hubId);
      const result = await runNodeExample([
        "--endpoint",
        hub.url,
        "--hub-id",
        hub.hubId,
        "--credential-file",
        credentialPath,
        "--timeout-ms",
        "250",
      ]);

      expect(result.code).toBe(0);
      expect(result.stderr).toBe("");
      expect(result.stdout).toContain("1 drive page(s)");
    } finally {
      await hub.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it.each([
    ["credential-file", "credential file JSON is invalid", "--credential-file"],
    ["invitation-file", "invitation file JSON is invalid", "--invitation-file"],
  ])(
    "reports malformed %s input without an uncaught parser error",
    async (_name, message, option) => {
      const directory = await mkdtemp(join(tmpdir(), "teslatlas-hub-example-"));
      const inputPath = join(directory, "input.json");

      try {
        await writeFile(inputPath, "{ malformed", "utf8");
        const result = await runNodeExample([
          "--endpoint",
          "http://127.0.0.1:1",
          "--hub-id",
          hubA,
          option,
          inputPath,
        ]);

        expect(result.code).toBe(1);
        expect(result.stderr).toBe(`Teslatlas Hub Node consumer: ${message}\n`);
        expect(result.stderr).not.toContain("SyntaxError");
        expect(result.stderr).not.toContain("at ");
      } finally {
        await rm(directory, { recursive: true, force: true });
      }
    },
  );

  it("reports an output persistence failure without overwriting an existing file", async () => {
    const hub = await startHubFixture();
    const directory = await mkdtemp(join(tmpdir(), "teslatlas-hub-example-"));
    const credentialPath = join(directory, "credential.json");
    const outputPath = join(directory, "new-credential.json");

    try {
      await writeCredential(credentialPath, hub.url, hub.hubId);
      await writeFile(outputPath, "keep this file\n", "utf8");
      const result = await runNodeExample([
        "--endpoint",
        hub.url,
        "--hub-id",
        hub.hubId,
        "--credential-file",
        credentialPath,
        "--credential-out",
        outputPath,
      ]);

      expect(result.code).toBe(1);
      expect(result.stderr).toBe(
        "Teslatlas Hub Node consumer: credential output cannot be created exclusively\n",
      );
      expect(await readFile(outputPath, "utf8")).toBe("keep this file\n");
    } finally {
      await hub.close();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("runs the browser consumer with caller-provisioned credentials, scopes them, and recovers after outage", async () => {
    const firstHub = await startHubFixture({ hubId: hubA });
    const secondHub = await startHubFixture({ hubId: hubB, rejectAuthorization: true });
    const browserServer = await startBrowserServer();
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();

    try {
      await page.goto(browserServer.url, { waitUntil: "domcontentloaded" });
      await fillConnection(page, firstHub, credentialFor(firstHub));
      await page.getByRole("button", { name: "Connect", exact: true }).click();
      await expectStatus(page, {
        connected: true,
        hubId: hubA,
        vehicleCount: 1,
        current: vehicleId,
        drives: "page",
      });
      await expectInputValue(page, "#credential", "");

      await fillConnection(page, secondHub, credentialFor(firstHub));
      await page.getByRole("button", { name: "Connect", exact: true }).click();
      await expectText(page, "credential_binding_mismatch");
      expect(secondHub.requests).toEqual([]);

      await page.locator("#credential").fill("");
      await page.getByRole("button", { name: "Connect", exact: true }).click();
      await expectText(
        page,
        "Authentication expired; provision a new credential through a pin-capable client.",
      );
      await expect
        .poll(() => page.getByRole("button", { name: "Refresh" }).isDisabled())
        .toBe(true);
      expect(
        secondHub.requests
          .filter((request) => request.path === "/v1/vehicles")
          .some((request) => request.authorization === undefined),
      ).toBe(true);
      expect(
        secondHub.requests
          .filter((request) => request.path === "/v1/vehicles")
          .every((request) => request.authorization !== `Bearer ${accessToken}`),
      ).toBe(true);

      await page.locator("#credential").fill(credentialFor(secondHub));
      await page.getByRole("button", { name: "Connect", exact: true }).click();
      await expectStatus(page, {
        connected: true,
        hubId: hubB,
        vehicleCount: 1,
        current: vehicleId,
        drives: "page",
      });

      secondHub.mode = "unauthorized";
      await page.getByRole("button", { name: "Refresh" }).click();
      await expectText(
        page,
        "Authentication expired; provision a new credential through a pin-capable client.",
      );
      await expectInputValue(page, "#credential", "");
      await expect
        .poll(() => page.getByRole("button", { name: "Refresh" }).isDisabled())
        .toBe(true);

      secondHub.mode = "normal";
      await page.locator("#credential").fill(credentialFor(secondHub));
      await page.getByRole("button", { name: "Connect", exact: true }).click();
      await expectStatus(page, {
        connected: true,
        hubId: hubB,
        vehicleCount: 1,
        current: vehicleId,
        drives: "page",
      });

      secondHub.mode = "outage";
      await page.getByRole("button", { name: "Refresh" }).click();
      await expectText(page, "service_unavailable (HTTP 503)");
      await expect
        .poll(() => page.getByRole("button", { name: "Refresh" }).isDisabled())
        .toBe(false);

      secondHub.mode = "normal";
      await page.getByRole("button", { name: "Refresh" }).click();
      await expectStatus(page, {
        connected: true,
        hubId: hubB,
        vehicleCount: 1,
        current: vehicleId,
        drives: "page",
      });
    } finally {
      await browser.close();
      await browserServer.close();
      await firstHub.close();
      await secondHub.close();
    }
  }, 45_000);
});

type RequestRecord = {
  readonly path: string;
  readonly authorization: string | undefined;
};

type HubFixture = {
  readonly server: Server;
  readonly url: string;
  readonly hubId: string;
  readonly requests: RequestRecord[];
  mode: "normal" | "unauthorized" | "outage";
  close(): Promise<void>;
};

async function startHubFixture(
  options: {
    capabilities?: string[];
    delayMs?: number;
    hubId?: string;
    rejectAuthorization?: boolean;
  } = {},
): Promise<HubFixture> {
  const [discovery, health, readiness, vehicles, current, drives] = await Promise.all(
    [
      "discovery.json",
      "health.json",
      "ready.json",
      "vehicles.json",
      "current.json",
      "drives.json",
    ].map(async (name) => JSON.parse(await readFile(new URL(name, fixtureRoot), "utf8"))),
  );
  const hubId = options.hubId ?? hubA;
  const fixtureAccessToken = hubId === hubB ? secondAccessToken : accessToken;
  discovery.hub_id = hubId;
  discovery.capabilities = options.capabilities ?? [
    "query.vehicles",
    "query.current",
    "query.drives",
    "sync.packs",
  ];
  const requests: RequestRecord[] = [];
  let mode: HubFixture["mode"] = "normal";
  const server = createServer((request, response) => {
    void (async () => {
      const origin = request.headers.origin;
      if (origin !== undefined) {
        response.setHeader("Access-Control-Allow-Origin", origin);
        response.setHeader(
          "Access-Control-Allow-Headers",
          "authorization, content-type, if-none-match",
        );
        response.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
        response.setHeader("Access-Control-Expose-Headers", "etag, x-request-id");
        response.setHeader("Vary", "Origin");
      }
      const pathname = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
      const authorization =
        typeof request.headers.authorization === "string"
          ? request.headers.authorization
          : undefined;
      requests.push({ path: pathname, authorization });
      request.resume();
      if (request.method === "OPTIONS") {
        response.writeHead(204).end();
        return;
      }
      if ((options.delayMs ?? 0) > 0) await wait(options.delayMs ?? 0);
      if (pathname === "/.well-known/teslatlas-hub") {
        sendJson(response, 200, discovery);
        return;
      }
      if (pathname === "/healthz") {
        sendJson(response, 200, health);
        return;
      }
      if (pathname === "/readyz") {
        sendJson(response, 200, readiness);
        return;
      }
      if (pathname === "/v1/vehicles") {
        if (
          mode === "unauthorized" ||
          authorization !== `Bearer ${fixtureAccessToken}` ||
          (options.rejectAuthorization === true && authorization === `Bearer ${accessToken}`)
        ) {
          response.writeHead(401).end();
          return;
        }
        sendJson(response, 200, vehicles);
        return;
      }
      if (pathname.endsWith("/current")) {
        sendJson(response, 200, current);
        return;
      }
      if (pathname.endsWith("/drives")) {
        if (mode === "outage") {
          sendJson(response, 503, {
            error: { code: "service_unavailable", message: "fixture outage" },
          });
          return;
        }
        response.setHeader("Cache-Control", "no-store");
        response.setHeader("ETag", '"fixture"');
        sendJson(response, 200, drives);
        return;
      }
      if (pathname.includes("/pairings/") && pathname.endsWith("/claim")) {
        sendJson(response, 200, {
          access_token: fixtureAccessToken,
          device_id: deviceId,
          expires_at_ms: Date.now() + 3_600_000,
        });
        return;
      }
      response.writeHead(404).end();
    })().catch(() => {
      if (!response.headersSent) response.writeHead(500);
      response.end();
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("fixture did not bind");
  return {
    server,
    url: `http://127.0.0.1:${address.port}`,
    hubId,
    requests,
    get mode() {
      return mode;
    },
    set mode(value) {
      mode = value;
    },
    close: () =>
      new Promise((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      ),
  };
}

async function writeCredential(path: string, endpoint: string, hubId: string): Promise<void> {
  await writeFile(
    path,
    `${JSON.stringify({
      endpoint,
      hubId,
      credential: {
        accessToken,
        deviceId,
        expiresAtMs: Date.now() + 3_600_000,
      },
    })}\n`,
    { encoding: "utf8", mode: 0o600 },
  );
}

function credentialFor(hub: HubFixture): string {
  return JSON.stringify({
    endpoint: hub.url,
    hubId: hub.hubId,
    credential: {
      accessToken: hub.hubId === hubB ? secondAccessToken : accessToken,
      deviceId,
      expiresAtMs: Date.now() + 3_600_000,
    },
  });
}

async function fillConnection(
  page: Awaited<ReturnType<Awaited<ReturnType<typeof chromium.launch>>["newPage"]>>,
  hub: HubFixture,
  credential: string,
): Promise<void> {
  await page.locator("#endpoint").fill(hub.url);
  await page.locator("#hub-id").fill(hub.hubId);
  await page.locator("#credential").fill(credential);
}

async function expectStatus(
  page: Awaited<ReturnType<Awaited<ReturnType<typeof chromium.launch>>["newPage"]>>,
  expected: object,
): Promise<void> {
  await expect
    .poll(async () => JSON.parse((await page.locator("#status").textContent()) ?? "{}"), {
      timeout: 10_000,
    })
    .toEqual(expected);
}

async function expectText(
  page: Awaited<ReturnType<Awaited<ReturnType<typeof chromium.launch>>["newPage"]>>,
  expected: string,
): Promise<void> {
  await expect.poll(() => page.locator("#status").textContent()).toBe(expected);
}

async function expectInputValue(
  page: Awaited<ReturnType<Awaited<ReturnType<typeof chromium.launch>>["newPage"]>>,
  selector: string,
  expected: string,
): Promise<void> {
  await expect.poll(() => page.locator(selector).inputValue()).toBe(expected);
}

async function startBrowserServer(): Promise<{
  readonly url: string;
  close(): Promise<void>;
}> {
  const child = spawn(process.execPath, ["serve.mjs"], {
    cwd: exampleRoot,
    env: { ...process.env, TESLATLAS_HUB_BROWSER_PORT: "0" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const url = await waitForUrl(child);
  return { url, close: () => stopProcess(child) };
}

async function runNodeExample(args: string[]): Promise<{
  readonly code: number | null;
  readonly stdout: string;
  readonly stderr: string;
}> {
  const child = spawn(process.execPath, ["node.mjs", ...args], {
    cwd: exampleRoot,
    env: { ...process.env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout?.setEncoding("utf8");
  child.stderr?.setEncoding("utf8");
  child.stdout?.on("data", (chunk: string) => {
    stdout += chunk;
  });
  child.stderr?.on("data", (chunk: string) => {
    stderr += chunk;
  });
  const [code] = await onceExit(child);
  return { code, stdout, stderr };
}

function waitForUrl(child: ChildProcess): Promise<string> {
  return new Promise((resolve, reject) => {
    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(() => finish(new Error("browser server did not start")), 10_000);
    const onStdout = (chunk: Buffer | string) => {
      stdout += chunk.toString();
      const match = stdout.match(/http:\/\/127\.0\.0\.1:\d+/u);
      if (match?.[0] !== undefined) finish(undefined, match[0]);
    };
    const onStderr = (chunk: Buffer | string) => {
      stderr += chunk.toString();
    };
    const onExit = (code: number | null) =>
      finish(new Error(`browser server exited with ${code}: ${stderr}`));
    const finish = (error?: Error, url?: string) => {
      clearTimeout(timeout);
      child.stdout?.off("data", onStdout);
      child.stderr?.off("data", onStderr);
      child.off("exit", onExit);
      if (error !== undefined) reject(error);
      else if (url !== undefined) resolve(url);
    };
    child.stdout?.on("data", onStdout);
    child.stderr?.on("data", onStderr);
    child.once("exit", onExit);
  });
}

function onceExit(child: ChildProcess): Promise<[number | null, NodeJS.Signals | null]> {
  return new Promise((resolve) => child.once("close", (code, signal) => resolve([code, signal])));
}

async function stopProcess(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = onceExit(child);
  child.kill("SIGTERM");
  await exited;
}

function sendJson(
  response: import("node:http").ServerResponse,
  status: number,
  value: unknown,
): void {
  const body = Buffer.from(JSON.stringify(value));
  response.writeHead(status, {
    "Content-Length": body.byteLength,
    "Content-Type": "application/json",
  });
  response.end(body);
}

function wait(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}
