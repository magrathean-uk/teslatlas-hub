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

interface TesatlasErrorOptions<TCode extends string> {
  readonly code: TCode;
  readonly status?: number;
  readonly requestId?: SafeRequestId;
}

export class TesatlasError<TCode extends string = string> extends Error {
  readonly code: TCode;
  readonly status?: number;
  readonly requestId?: SafeRequestId;

  protected constructor(message: string, options: TesatlasErrorOptions<TCode>) {
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

export class InvalidProtocolErrorCodeError extends TesatlasError<"invalid_protocol_error_code"> {
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

export class InvalidSafeRequestIdError extends TesatlasError<"invalid_safe_request_id"> {
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

export class ProtocolError extends TesatlasError<ProtocolErrorCode> {
  constructor(options: ProtocolErrorOptions) {
    super("Tesatlas protocol request failed", {
      code: options.code,
      status: options.status,
      ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    });
  }
}

export class IncompatibleProtocolError extends TesatlasError<"incompatible_protocol"> {
  constructor() {
    super("Tesatlas protocol versions are incompatible", {
      code: "incompatible_protocol",
    });
  }
}

export class MissingCapabilityError extends TesatlasError<"missing_capability"> {
  readonly capability: CapabilityId;

  constructor(capability: CapabilityId) {
    super("Tesatlas capability is unavailable", { code: "missing_capability" });
    this.capability = capability;
  }
}

export class ProtocolValidationError extends TesatlasError<"protocol_validation"> {
  readonly validator: string;

  constructor(validator: string) {
    super("Tesatlas protocol response is invalid", { code: "protocol_validation" });
    this.validator = validator;
  }
}

export class ProtocolHttpError extends TesatlasError<ProtocolErrorCode> {
  readonly retryable: boolean;
  readonly retryAfterSeconds?: number;

  constructor(
    options: ProtocolErrorOptions & {
      readonly retryable: boolean;
      readonly retryAfterSeconds?: number;
    },
  ) {
    super("Tesatlas protocol request failed", {
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

export class ReplayGapError extends TesatlasError<"event_replay_expired"> {
  constructor(status: number, requestId?: SafeRequestId) {
    super("Tesatlas event replay point expired", {
      code: "event_replay_expired",
      status,
      ...(requestId === undefined ? {} : { requestId }),
    });
  }
}

export class CommandUncertainError extends TesatlasError<"command_uncertain"> {
  constructor() {
    super("Tesatlas command submission outcome is uncertain", {
      code: "command_uncertain",
    });
  }
}

export class TransportError extends TesatlasError<"transport_error"> {
  constructor() {
    super("Tesatlas transport request failed", { code: "transport_error" });
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
