import { containsControlCharacters, TeslatlasError } from "../core/errors.js";

export interface UncheckedCommandSafety {
  readonly idempotencyKey: string;
  readonly retry: unknown;
}

export interface CommandSafety {
  readonly idempotencyKey: string;
  readonly retry: "never";
}

export type CommandSafetyFailure =
  | "missing-idempotency-key"
  | "invalid-idempotency-key"
  | "automatic-retry-forbidden";

export class CommandSafetyError extends TeslatlasError<"unsafe_command_submission"> {
  readonly reason: CommandSafetyFailure;

  constructor(reason: CommandSafetyFailure) {
    super("Command submission requires an idempotency key and retry mode never", {
      code: "unsafe_command_submission",
    });
    this.reason = reason;
  }
}

export function assertCommandSafety(
  safety: UncheckedCommandSafety,
): asserts safety is CommandSafety {
  if (safety.idempotencyKey.trim().length === 0) {
    throw new CommandSafetyError("missing-idempotency-key");
  }
  if (containsControlCharacters(safety.idempotencyKey)) {
    throw new CommandSafetyError("invalid-idempotency-key");
  }
  if (safety.retry !== "never") {
    throw new CommandSafetyError("automatic-retry-forbidden");
  }
}
