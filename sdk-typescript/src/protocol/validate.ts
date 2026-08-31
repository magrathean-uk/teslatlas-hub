import { ProtocolValidationError } from "../core/errors.js";

export type ProtocolValidator = (value: unknown) => boolean;

export function decodeProtocolValue<T>(
  value: unknown,
  validator: ProtocolValidator,
  validatorName: string,
): T {
  if (!validator(value)) {
    throw new ProtocolValidationError(validatorName);
  }
  return value as T;
}
