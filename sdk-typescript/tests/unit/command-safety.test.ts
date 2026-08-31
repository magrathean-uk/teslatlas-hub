import { describe, expect, it } from "vitest";
import { asIdempotencyKey, InvalidIdempotencyKeyError } from "../../src/commands/idempotency.js";
import { CommandSafetyError, assertCommandSafety } from "../../src/commands/safety.js";

describe("command submission safety", () => {
  it("accepts an explicit idempotency key only with retry disabled", () => {
    const safety: { idempotencyKey: string; retry: unknown } = {
      idempotencyKey: "request-7",
      retry: "never",
    };

    assertCommandSafety(safety);

    expect(safety.retry).toBe("never");
  });

  it.each([
    { idempotencyKey: "", retry: "never" },
    { idempotencyKey: "   ", retry: "never" },
    { idempotencyKey: "request-7\r\nInjected: true", retry: "never" },
    { idempotencyKey: "request-7", retry: "automatic" },
  ])("rejects unsafe command options %#", (safety) => {
    expect(() => assertCommandSafety(safety)).toThrow(CommandSafetyError);
  });

  it("accepts only a protocol UUID idempotency key", () => {
    expect(asIdempotencyKey("11111111-1111-4111-8111-111111111111")).toBe(
      "11111111-1111-4111-8111-111111111111",
    );
    expect(() => asIdempotencyKey("request-7")).toThrow(InvalidIdempotencyKeyError);
    expect(() => asIdempotencyKey("11111111-1111-4111-8111-111111111111\r\nX: y")).toThrow(
      InvalidIdempotencyKeyError,
    );
  });

  it("rejects a forged non-string idempotency key with the protocol error", () => {
    expect(() => asIdempotencyKey(Symbol("idempotency") as never)).toThrow(
      InvalidIdempotencyKeyError,
    );
  });
});
