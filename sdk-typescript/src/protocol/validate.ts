import { ProtocolValidationError } from "../core/errors.js";

import { assertFiniteNumbers } from "../http/bounded-json.js";

export type ProtocolValidator = (value: unknown) => boolean;

export function decodeProtocolValue<T>(
  value: unknown,
  validator: ProtocolValidator,
  validatorName: string,
): T {
  assertFiniteNumbers(value, validatorName);
  if (!validator(value)) {
    throw new ProtocolValidationError(validatorName);
  }
  return value as T;
}
