import { TeslatlasError } from "../core/errors.js";

const idempotencyKeyBrand: unique symbol = Symbol("IdempotencyKey");
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

export type IdempotencyKey = string & {
  readonly [idempotencyKeyBrand]: true;
};

export class InvalidIdempotencyKeyError extends TeslatlasError<"invalid_idempotency_key"> {
  constructor() {
    super("Idempotency key must use the protocol UUID syntax", {
      code: "invalid_idempotency_key",
    });
  }
}

export function asIdempotencyKey(value: string): IdempotencyKey {
  if (typeof value !== "string" || !uuidPattern.test(value)) {
    throw new InvalidIdempotencyKeyError();
  }
  return value as IdempotencyKey;
}
