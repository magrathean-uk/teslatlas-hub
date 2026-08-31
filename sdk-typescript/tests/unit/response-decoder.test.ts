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
