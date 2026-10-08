import { createHash, X509Certificate } from "node:crypto";
import { readFile } from "node:fs/promises";
import { createServer, type Server } from "node:https";
import { describe, expect, it } from "vitest";
import { createHubClient } from "../../src/node.js";
import type { HubCredential, HubCredentialStore, HubInvitation } from "../../src/index.js";

const hubId = "11111111-1111-4111-8111-111111111111";
const secret = "7".repeat(64);
const certificatePath = new URL("../fixtures/tls/localhost-cert.pem", import.meta.url);
const privateKeyPath = new URL("../fixtures/tls/localhost-key.pem", import.meta.url);
const discoveryPath = new URL(
  "../../protocol/source/profiles/hub-http-v1/1.0.0/examples/discovery.json",
  import.meta.url,
);

class MemoryCredentials implements HubCredentialStore {
  credential: HubCredential | undefined;

  load(): HubCredential | undefined {
    return this.credential;
  }

  save(credential: HubCredential): void {
    this.credential = credential;
  }

  clear(): void {
    this.credential = undefined;
  }
}

describe("Node current-Hub invitation pinning", () => {
  it("validates normal TLS and the connected leaf DER pin before sending a claim", async () => {
    const fixture = await startClaimServer();
    try {
      const credentials = new MemoryCredentials();
      const client = createHubClient({
        endpoint: fixture.endpoint,
        expectedHubId: hubId,
        credentials,
        fetch: discoveryFetch,
        nodeTls: { ca: fixture.certificate },
      });

      await expect(client.claimPairing(fixture.invitation, "Pinned Node")).resolves.toMatchObject({
        value: { deviceId: hubId },
      });

      expect(fixture.claimBodies).toEqual([JSON.stringify({ secret, device_name: "Pinned Node" })]);
      expect(credentials.credential?.accessToken).toBe("9".repeat(64));
    } finally {
      await fixture.close();
    }
  });

  it("rejects a leaf-pin mismatch without sending the claim secret even when the process disables TLS verification", async () => {
    const fixture = await startClaimServer("0".repeat(64));
    const previousTlsSetting = process.env.NODE_TLS_REJECT_UNAUTHORIZED;
    process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";
    try {
      const client = createHubClient({
        endpoint: fixture.endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: discoveryFetch,
        nodeTls: { ca: fixture.certificate },
      });

      const error = await captureError(client.claimPairing(fixture.invitation, "Wrong pin"));

      expect(error).toMatchObject({ code: "hub_tls_pin_mismatch" });
      expect(JSON.stringify(error)).not.toContain(secret);
      expect(String(error)).not.toContain(secret);
      expect(fixture.claimBodies).toEqual([]);
    } finally {
      if (previousTlsSetting === undefined) delete process.env.NODE_TLS_REJECT_UNAUTHORIZED;
      else process.env.NODE_TLS_REJECT_UNAUTHORIZED = previousTlsSetting;
      await fixture.close();
    }
  });

  it("rejects a missing invitation pin before discovery or claim network I/O", async () => {
    const fixture = await startClaimServer();
    let discoveryRequests = 0;
    try {
      const invitation = { ...fixture.invitation } as Record<string, unknown>;
      delete invitation.tlsPin;
      const client = createHubClient({
        endpoint: fixture.endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: async () => {
          discoveryRequests += 1;
          return discoveryFetch();
        },
        nodeTls: { ca: fixture.certificate },
      });

      await expect(
        client.claimPairing(invitation as unknown as HubInvitation, "Missing pin"),
      ).rejects.toMatchObject({
        code: "protocol_validation",
        validator: "HubInvitation",
      });
      expect(discoveryRequests).toBe(0);
      expect(fixture.claimBodies).toEqual([]);
    } finally {
      await fixture.close();
    }
  });

  it("maps ordinary TLS trust failure to a bounded transport error without sending the secret", async () => {
    const fixture = await startClaimServer();
    try {
      const client = createHubClient({
        endpoint: fixture.endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: discoveryFetch,
      });

      const error = await captureError(client.claimPairing(fixture.invitation, "Untrusted CA"));

      expect(error).toMatchObject({ code: "transport_error" });
      expect(error).not.toHaveProperty("cause");
      expect(JSON.stringify(error)).not.toContain(secret);
      expect(String(error)).not.toContain(secret);
      expect(fixture.claimBodies).toEqual([]);
    } finally {
      await fixture.close();
    }
  });

  it.each([204, 205, 304])(
    "rejects a null-body claim status without an uncaught exception: %i",
    async (status) => {
      const fixture = await startClaimServer(undefined, status);
      try {
        const client = createHubClient({
          endpoint: fixture.endpoint,
          expectedHubId: hubId,
          credentials: new MemoryCredentials(),
          fetch: discoveryFetch,
          nodeTls: { ca: fixture.certificate },
        });

        await expect(client.claimPairing(fixture.invitation, "Null body")).rejects.toMatchObject({
          code: "hub_http_error",
          status,
        });
      } finally {
        await fixture.close();
      }
    },
  );

  it("rejects an out-of-range HTTP status as a protocol error", async () => {
    const fixture = await startClaimServer(undefined, 700);
    try {
      const client = createHubClient({
        endpoint: fixture.endpoint,
        expectedHubId: hubId,
        credentials: new MemoryCredentials(),
        fetch: discoveryFetch,
        nodeTls: { ca: fixture.certificate },
      });

      await expect(client.claimPairing(fixture.invitation, "Invalid status")).rejects.toMatchObject(
        {
          code: "protocol_validation",
          validator: "HubResponse.status",
        },
      );
    } finally {
      await fixture.close();
    }
  });
});

async function startClaimServer(
  invitationPin?: string,
  responseStatus = 200,
): Promise<{
  readonly certificate: string;
  readonly claimBodies: string[];
  readonly close: () => Promise<void>;
  readonly endpoint: string;
  readonly invitation: HubInvitation;
}> {
  const [certificate, key] = await Promise.all([
    readFile(certificatePath, "utf8"),
    readFile(privateKeyPath, "utf8"),
  ]);
  const claimBodies: string[] = [];
  const server = createServer({ cert: certificate, key }, (request, response) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => {
      claimBodies.push(Buffer.concat(chunks).toString("utf8"));
      response.writeHead(responseStatus, { "Content-Type": "application/json" });
      response.end(
        responseStatus === 200
          ? JSON.stringify({
              access_token: "9".repeat(64),
              device_id: hubId,
              expires_at_ms: Date.now() + 60_000,
            })
          : "malformed response",
      );
    });
  });
  server.on("tlsClientError", () => undefined);
  await listen(server);
  const address = server.address();
  if (address === null || typeof address === "string")
    throw new Error("HTTPS fixture did not bind");
  const endpoint = `https://127.0.0.1:${address.port}`;
  const actualPin = createHash("sha256").update(new X509Certificate(certificate).raw).digest("hex");
  const tlsPin = invitationPin ?? actualPin;
  const pairingId = hubId;
  const pairingUri = `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${pairingId}&secret=${secret}&tls_pin=${tlsPin}`;
  return {
    certificate,
    claimBodies,
    endpoint,
    invitation: {
      endpoint,
      expiresAtMs: Date.now() + 60_000,
      pairingId,
      pairingUri,
      secret,
      tlsPin,
    },
    close: () => close(server),
  };
}

async function discoveryFetch(): Promise<Response> {
  return new Response(await readFile(discoveryPath, "utf8"), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}

function listen(server: Server): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function close(server: Server): Promise<void> {
  return new Promise((resolve, reject) => {
    server.close((error) => (error === undefined ? resolve() : reject(error)));
  });
}
