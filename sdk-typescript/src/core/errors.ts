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
