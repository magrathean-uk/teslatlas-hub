import { createServer, type Server } from "node:http";
import type { Socket } from "node:net";
import currentState from "../../protocol/source/examples/current-state.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { validateCurrentState } from "../../src/generated/validators.js";
import { decodeReadResponse } from "../../src/http/response-decoder.js";
import type { CurrentState } from "../../src/protocol/models.js";

const operationTimeoutMilliseconds = 1_000;
const maximumRejectedStreamLifetimeMilliseconds = 3_000;

interface ClosedResponse {
  readonly writableEnded: boolean;
  readonly fixtureCleanupStarted: boolean;
}

describe("native Fetch response cleanup", () => {
  it("closes a MIME-rejected loopback stream before its finite end and then reads a valid response", async () => {
    const fixture = await startFixture();
    try {
      const rejectedResponse = await within(
        globalThis.fetch(`${fixture.origin}/rejected`, { signal: fixture.signal }),
        "Fetch did not receive rejected response headers",
      );
      let validatorCalls = 0;
      await expect(
        within(
          decodeReadResponse<CurrentState>(
            rejectedResponse,
            (value) => {
              validatorCalls += 1;
              return validateCurrentState(value);
            },
            "validateCurrentState",
          ),
          "Wrong-MIME admission did not reject promptly",
        ),
      ).rejects.toMatchObject({
        code: "protocol_validation",
        validator: "validateCurrentState.contentType",
      });
      expect(validatorCalls).toBe(0);

      // Observe close before teardown or the fixture's later normal end can
      // produce it. Keeping this Response alive also rules out GC as the owner.
      const closed = await within(
        fixture.rejectedClosed,
        "MIME-rejected response stayed open after SDK rejection",
      );
      expect(closed).toEqual({ writableEnded: false, fixtureCleanupStarted: false });
      expect(rejectedResponse.bodyUsed).toBe(true);
      expect(rejectedResponse.body?.locked).toBe(false);

      const successfulResponse = await within(
        globalThis.fetch(`${fixture.origin}/valid`, { signal: fixture.signal }),
        "Fetch did not receive the subsequent valid response",
      );
      await expect(
        within(
          decodeReadResponse<CurrentState>(
            successfulResponse,
            validateCurrentState,
            "validateCurrentState",
          ),
          "Subsequent valid response did not finish decoding",
        ),
      ).resolves.toEqual({
        kind: "modified",
        value: currentState,
        metadata: { status: 200, etag: 'W/"native-control"' },
      });
      expect(fixture.requests).toEqual(["/rejected", "/valid"]);
    } finally {
      await fixture.close();
    }
  }, 8_000);
});

async function startFixture() {
  const controller = new AbortController();
  const sockets = new Set<Socket>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const requests: string[] = [];
  let cleanupStarted = false;
  let resolveRejectedClosed!: (closed: ClosedResponse) => void;
  const rejectedClosed = new Promise<ClosedResponse>((resolve) => {
    resolveRejectedClosed = resolve;
  });
  const server = createServer((request, response) => {
    requests.push(request.url ?? "");
    if (request.url === "/valid") {
      response.writeHead(200, {
        "Content-Type": "application/json",
        ETag: 'W/"native-control"',
      });
      response.end(JSON.stringify(currentState));
      return;
    }
    if (request.url !== "/rejected") {
      response.writeHead(404);
      response.end();
      return;
    }

    response.writeHead(200, { "Content-Type": "text/plain" });
    response.flushHeaders();
    response.write("synthetic wrong-MIME body\n");
    const writeTimer = setInterval(() => {
      if (!response.destroyed && !response.writableEnded) {
        response.write("synthetic bounded streaming chunk\n");
      }
    }, 25);
    const endTimer = setTimeout(() => {
      timers.delete(endTimer);
      response.end();
    }, maximumRejectedStreamLifetimeMilliseconds);
    timers.add(writeTimer);
    timers.add(endTimer);
    response.once("close", () => {
      clearInterval(writeTimer);
      clearTimeout(endTimer);
      timers.delete(writeTimer);
      timers.delete(endTimer);
      resolveRejectedClosed({
        writableEnded: response.writableEnded,
        fixtureCleanupStarted: cleanupStarted,
      });
    });
  });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });

  const close = async () => {
    cleanupStarted = true;
    for (const timer of timers) clearTimeout(timer);
    timers.clear();
    controller.abort(new Error("Owned native Fetch fixture finished"));
    await closeServer(server, sockets);
  };

  try {
    await within(
      new Promise<void>((resolve, reject) => {
        const onError = (error: Error) => {
          server.off("listening", onListening);
          reject(error);
        };
        const onListening = () => {
          server.off("error", onError);
          resolve();
        };
        server.once("error", onError);
        server.once("listening", onListening);
        server.listen({ host: "127.0.0.1", port: 0, signal: controller.signal });
      }),
      "Loopback fixture did not start listening",
    );
    const address = server.address();
    if (address === null || typeof address === "string") {
      throw new Error("Loopback fixture did not receive an ephemeral TCP port");
    }
    return {
      origin: `http://127.0.0.1:${address.port}`,
      signal: controller.signal,
      rejectedClosed,
      requests,
      close,
    };
  } catch (error) {
    await close();
    throw error;
  }
}

async function closeServer(server: Server, sockets: ReadonlySet<Socket>): Promise<void> {
  const closed = new Promise<void>((resolve, reject) => {
    server.close((error?: Error) => {
      if (
        error !== undefined &&
        (error as NodeJS.ErrnoException).code !== "ERR_SERVER_NOT_RUNNING"
      ) {
        reject(error);
      } else {
        resolve();
      }
    });
  });
  server.closeAllConnections();
  for (const socket of sockets) socket.destroy();
  await within(closed, "Owned loopback fixture did not finish closing");
}

async function within<T>(pending: Promise<T>, message: string): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      pending,
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => reject(new Error(message)), operationTimeoutMilliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}
