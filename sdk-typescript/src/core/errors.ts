import type { CapabilityId } from "./capabilities.js";

const protocolErrorCodeBrand: unique symbol = Symbol("ProtocolErrorCode");
const safeRequestIdBrand: unique symbol = Symbol("SafeRequestId");
const maximumSafeRequestIdLength = 256;

export type ProtocolErrorCode = string & {
  readonly [protocolErrorCodeBrand]: true;
};

export type SafeRequestId = string & {
  readonly [safeRequestIdBrand]: true;
};

interface TeslatlasErrorOptions<TCode extends string> {
  readonly code: TCode;
  readonly status?: number;
  readonly requestId?: SafeRequestId;
}

export class TeslatlasError<TCode extends string = string> extends Error {
  readonly code: TCode;
  readonly status?: number;
  readonly requestId?: SafeRequestId;

  protected constructor(message: string, options: TeslatlasErrorOptions<TCode>) {
    super(message);
    this.name = new.target.name;
    this.code = options.code;
    if (options.status !== undefined) {
      this.status = options.status;
    }
    if (options.requestId !== undefined) {
      this.requestId = options.requestId;
    }
  }
}

export class InvalidProtocolErrorCodeError extends TeslatlasError<"invalid_protocol_error_code"> {
  constructor() {
    super("Protocol error code must be non-empty and contain no control characters", {
      code: "invalid_protocol_error_code",
    });
  }
}

export function asProtocolErrorCode(value: string): ProtocolErrorCode {
  if (value.length === 0 || containsControlCharacters(value)) {
    throw new InvalidProtocolErrorCodeError();
  }
  return value as ProtocolErrorCode;
}

export class InvalidSafeRequestIdError extends TeslatlasError<"invalid_safe_request_id"> {
  constructor() {
    super("Request ID must be 1 to 256 characters and contain no control characters", {
      code: "invalid_safe_request_id",
    });
  }
}

export function asSafeRequestId(value: string): SafeRequestId {
  if (
    value.length === 0 ||
    value.length > maximumSafeRequestIdLength ||
    containsControlCharacters(value)
  ) {
    throw new InvalidSafeRequestIdError();
  }
  return value as SafeRequestId;
}

export interface ProtocolErrorOptions {
  readonly code: ProtocolErrorCode;
  readonly status: number;
  readonly requestId?: SafeRequestId;
}

export class ProtocolError extends TeslatlasError<ProtocolErrorCode> {
  constructor(options: ProtocolErrorOptions) {
    super("Teslatlas protocol request failed", {
      code: options.code,
      status: options.status,
      ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    });
  }
}

export class IncompatibleProtocolError extends TeslatlasError<"incompatible_protocol"> {
  constructor() {
    super("Teslatlas protocol versions are incompatible", {
      code: "incompatible_protocol",
    });
  }
}

export class MissingCapabilityError extends TeslatlasError<"missing_capability"> {
  readonly capability: CapabilityId;

  constructor(capability: CapabilityId) {
    super("Teslatlas capability is unavailable", { code: "missing_capability" });
    this.capability = capability;
  }
}

export class ProtocolValidationError extends TeslatlasError<"protocol_validation"> {
  readonly validator: string;

  constructor(validator: string) {
    super("Teslatlas protocol response is invalid", { code: "protocol_validation" });
    this.validator = validator;
  }
}

export class ProtocolHttpError extends TeslatlasError<ProtocolErrorCode> {
  readonly retryable: boolean;
  readonly retryAfterSeconds?: number;

  constructor(
    options: ProtocolErrorOptions & {
      readonly retryable: boolean;
      readonly retryAfterSeconds?: number;
    },
  ) {
    super("Teslatlas protocol request failed", {
      code: options.code,
      status: options.status,
      ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    });
    this.retryable = options.retryable;
    if (options.retryAfterSeconds !== undefined) {
      this.retryAfterSeconds = options.retryAfterSeconds;
    }
  }
}

export class ReplayGapError extends TeslatlasError<"event_replay_expired"> {
  constructor(status: number, requestId?: SafeRequestId) {
    super("Teslatlas event replay point expired", {
      code: "event_replay_expired",
      status,
      ...(requestId === undefined ? {} : { requestId }),
    });
  }
}

export class CommandUncertainError extends TeslatlasError<"command_uncertain"> {
  constructor() {
    super("Teslatlas command submission outcome is uncertain", {
      code: "command_uncertain",
    });
  }
}

export class TransportError extends TeslatlasError<"transport_error"> {
  constructor() {
    super("Teslatlas transport request failed", { code: "transport_error" });
  }
}

/** @internal */
export function containsControlCharacters(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (codePoint !== undefined && (codePoint <= 31 || codePoint === 127)) {
      return true;
    }
  }
  return false;
}
