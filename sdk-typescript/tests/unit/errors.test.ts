import { describe, expect, it } from "vitest";
import {
  CommandUncertainError,
  IncompatibleProtocolError,
  InvalidProtocolErrorCodeError,
  InvalidSafeRequestIdError,
  MissingCapabilityError,
  ProtocolError,
  ProtocolHttpError,
  ProtocolValidationError,
  ReplayGapError,
  asProtocolErrorCode,
  asSafeRequestId,
} from "../../src/core/errors.js";
import { asCapabilityId } from "../../src/core/capabilities.js";

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

  it("exposes only safe protocol negotiation and validation fields", () => {
    expect(new IncompatibleProtocolError()).toMatchObject({
      code: "incompatible_protocol",
      message: "Teslatlas protocol versions are incompatible",
    });
    expect(new MissingCapabilityError(asCapabilityId("commands.async"))).toMatchObject({
      code: "missing_capability",
      capability: "commands.async",
      message: "Teslatlas capability is unavailable",
    });
    expect(new ProtocolValidationError("validateDiscovery")).toMatchObject({
      code: "protocol_validation",
      validator: "validateDiscovery",
      message: "Teslatlas protocol response is invalid",
    });
  });

  it("exposes retry metadata and safe request IDs without a response payload", () => {
    const error = new ProtocolHttpError({
      code: asProtocolErrorCode("rate_limited"),
      status: 429,
      requestId: asSafeRequestId("safe-request-id"),
      retryable: true,
      retryAfterSeconds: 30,
    });

    expect(error).toMatchObject({
      code: "rate_limited",
      status: 429,
      requestId: "safe-request-id",
      retryable: true,
      retryAfterSeconds: 30,
      message: "Teslatlas protocol request failed",
    });
    expect(error).not.toHaveProperty("body");
    expect(error).not.toHaveProperty("headers");
    expect(error).not.toHaveProperty("cause");
  });

  it("uses static replay-gap and uncertain-command errors", () => {
    expect(new ReplayGapError(409, asSafeRequestId("safe-request-id"))).toMatchObject({
      code: "event_replay_expired",
      status: 409,
      requestId: "safe-request-id",
      message: "Teslatlas event replay point expired",
    });
    expect(new CommandUncertainError()).toMatchObject({
      code: "command_uncertain",
      message: "Teslatlas command submission outcome is uncertain",
    });
  });
});
