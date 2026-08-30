import { describe, expect, it } from "vitest";
import {
  InvalidProtocolErrorCodeError,
  InvalidSafeRequestIdError,
  ProtocolError,
  asProtocolErrorCode,
  asSafeRequestId,
} from "../../src/core/errors.js";

describe("stable SDK errors", () => {
  it("preserves an opaque protocol code and explicitly safe request ID", () => {
    const error = new ProtocolError({
      code: asProtocolErrorCode("rate_limited"),
      status: 429,
      requestId: asSafeRequestId("safe-request-id"),
    });

    expect(error).toMatchObject({
      name: "ProtocolError",
      code: "rate_limited",
      status: 429,
      requestId: "safe-request-id",
      message: "Teslatlas protocol request failed",
    });
  });

  it("rejects empty and control-character protocol codes", () => {
    expect(() => asProtocolErrorCode("")).toThrow(InvalidProtocolErrorCodeError);
    expect(() => asProtocolErrorCode("bad\ncode")).toThrow(InvalidProtocolErrorCodeError);
  });

  it("rejects empty, control-character, and oversized request IDs", () => {
    expect(() => asSafeRequestId("")).toThrow(InvalidSafeRequestIdError);
    expect(() => asSafeRequestId("request\r\nid")).toThrow(InvalidSafeRequestIdError);
    expect(() => asSafeRequestId("r".repeat(257))).toThrow(InvalidSafeRequestIdError);
  });
});
