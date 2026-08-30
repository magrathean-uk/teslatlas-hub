import { describe, expect, it } from "vitest";
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
});
