import currentState from "../../protocol/source/examples/current-state.json" with { type: "json" };
import problem from "../../protocol/source/examples/error.json" with { type: "json" };
import { describe, expect, it } from "vitest";
import { ProtocolHttpError, ProtocolValidationError } from "../../src/core/errors.js";
import { validateCurrentState } from "../../src/generated/validators.js";
import { decodeReadResponse } from "../../src/http/response-decoder.js";
import type { CurrentState } from "../../src/protocol/models.js";

describe("read response decoder", () => {
  it("parses and validates one JSON success", async () => {
    const result = await decodeReadResponse<CurrentState>(
      Response.json(currentState, {
        headers: {
          ETag: 'W/"revision-8"',
          "Teslatlas-Protocol-Version": "1.2.0",
          "X-Request-ID": "request_demo_0001",
        },
      }),
      validateCurrentState,
      "validateCurrentState",
    );

    expect(result.kind).toBe("modified");
    if (result.kind === "modified") {
      expect(result.value.vehicle_id).toBe("vehicle_demo_alpha");
      expect(result.metadata).toEqual({
        status: 200,
        etag: 'W/"revision-8"',
        requestId: "request_demo_0001",
        protocolVersion: "1.2.0",
      });
    }
  });

  it("requires an ordinary ETag on every JSON 200 response", async () => {
    await expect(
      decodeReadResponse<CurrentState>(
        Response.json(currentState, { headers: { ETag: 'W/"revision-8"' } }),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).resolves.toMatchObject({ kind: "modified", metadata: { etag: 'W/"revision-8"' } });

    await expect(
      decodeReadResponse<CurrentState>(
        Response.json(currentState),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
  });

  it("maps an empty 304 only when it carries a valid ETag", async () => {
    await expect(
      decodeReadResponse<CurrentState>(
        new Response(null, { status: 304, headers: { ETag: 'W/"revision-8"' } }),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).resolves.toEqual({
      kind: "not-modified",
      metadata: { status: 304, etag: 'W/"revision-8"' },
    });

    await expect(
      decodeReadResponse<CurrentState>(
        new Response(null, { status: 304 }),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
  });

  it.each([200, 304])("rejects malformed ETag metadata on status %i", async (status) => {
    const response =
      status === 200
        ? Response.json(currentState, { headers: { ETag: "not-an-etag" } })
        : new Response(null, { status, headers: { ETag: "not-an-etag" } });

    await expect(
      decodeReadResponse<CurrentState>(response, validateCurrentState, "validateCurrentState"),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
  });

  it.each([200, 304])(
    "accepts a long protocol-valid response ETag on status %i",
    async (status) => {
      const etag = `"${"x".repeat(512)}"`;
      const response =
        status === 200
          ? Response.json(currentState, { headers: { ETag: etag } })
          : new Response(null, { status, headers: { ETag: etag } });

      await expect(
        decodeReadResponse<CurrentState>(response, validateCurrentState, "validateCurrentState"),
      ).resolves.toMatchObject({ metadata: { status, etag } });
    },
  );

  it("decodes safe problem fields without retaining sensitive detail or response", async () => {
    const sensitiveProblem = {
      ...problem,
      detail: "Bearer must-not-leak",
    };
    const error = await captureError(
      decodeReadResponse<CurrentState>(
        Response.json(sensitiveProblem, {
          status: 400,
          headers: { "Content-Type": "application/problem+json" },
        }),
        validateCurrentState,
        "validateCurrentState",
      ),
    );

    expect(error).toBeInstanceOf(ProtocolHttpError);
    expect(error).toMatchObject({
      code: "invalid_cursor",
      status: 400,
      requestId: "request_demo_0001",
      retryable: false,
    });
    expect(error).not.toHaveProperty("body");
    expect(error).not.toHaveProperty("response");
    expect(error).not.toHaveProperty("cause");
    expect(String(error)).not.toContain("must-not-leak");
  });

  it("derives retry fields from a retryable problem and declared Retry-After header only", async () => {
    const retryableProblem = {
      ...problem,
      status: 429,
      code: "concurrency_limit",
      retryable: true,
    };
    const retryable = await captureError(
      decodeReadResponse<CurrentState>(
        Response.json(retryableProblem, {
          status: 429,
          headers: {
            "Content-Type": "application/problem+json",
            "Retry-After": "1",
          },
        }),
        validateCurrentState,
        "validateCurrentState",
      ),
    );
    const nonRetryable = await captureError(
      decodeReadResponse<CurrentState>(
        Response.json(problem, {
          status: 400,
          headers: { "Content-Type": "application/problem+json" },
        }),
        validateCurrentState,
        "validateCurrentState",
      ),
    );

    expect(retryable).toMatchObject({ retryable: true, retryAfterSeconds: 1 });
    expect(nonRetryable).toMatchObject({ retryable: false, retryAfterSeconds: undefined });
  });

  it("rejects wrong media types, malformed JSON, and mismatched problem status", async () => {
    await expect(
      decodeReadResponse<CurrentState>(
        new Response(JSON.stringify(currentState), {
          headers: { "Content-Type": "text/plain" },
        }),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).rejects.toBeInstanceOf(ProtocolValidationError);

    await expect(
      decodeReadResponse<CurrentState>(
        new Response("not-json", { headers: { "Content-Type": "application/json" } }),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).rejects.toBeInstanceOf(ProtocolValidationError);

    await expect(
      decodeReadResponse<CurrentState>(
        Response.json(problem, {
          status: 403,
          headers: { "Content-Type": "application/problem+json" },
        }),
        validateCurrentState,
        "validateCurrentState",
      ),
    ).rejects.toBeInstanceOf(ProtocolValidationError);
  });
});

async function captureError(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
    return undefined;
  } catch (error) {
    return error;
  }
}
