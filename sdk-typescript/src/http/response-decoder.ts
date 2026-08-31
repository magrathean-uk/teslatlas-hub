import type { ReadResult, ResponseMetadata, WriteResult } from "../client/operations.js";
import {
  asProtocolErrorCode,
  asSafeRequestId,
  containsControlCharacters,
  ProtocolHttpError,
  ProtocolValidationError,
} from "../core/errors.js";
import { asEntityTag } from "../core/opaque-values.js";
import { validateProblem } from "../generated/validators.js";
import type { ProtocolProblem } from "../protocol/models.js";
import { decodeProtocolValue, type ProtocolValidator } from "../protocol/validate.js";
import { isStrongEntityTag } from "./strong-etag.js";

const protocolVersionPattern = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u;
const safeLocationBase = "https://teslatlas-location.invalid";

interface ResponseMetadataRequirements {
  readonly requireEntityTag?: boolean;
  readonly requireStrongEntityTag?: boolean;
  readonly requireLocation?: boolean;
}

export interface ReadResponseRequirements {
  readonly requireStrongEntityTag?: boolean;
}

export interface WriteResponseRequirements extends ResponseMetadataRequirements {
  readonly successStatus: 200 | 201 | 202;
}

export async function decodeReadResponse<T>(
  response: Response,
  validator: ProtocolValidator,
  validatorName: string,
  signal?: AbortSignal,
  requirements: ReadResponseRequirements = {},
): Promise<ReadResult<T>> {
  const metadataRequirements: ResponseMetadataRequirements = {
    requireEntityTag: true,
    ...(requirements.requireStrongEntityTag === true ? { requireStrongEntityTag: true } : {}),
  };
  if (response.status === 304) {
    if (response.body !== null) throw new ProtocolValidationError(`${validatorName}.304`);
    return {
      kind: "not-modified",
      metadata: readResponseMetadata(response, validatorName, metadataRequirements),
    };
  }

  if (response.status === 200) {
    return {
      kind: "modified",
      ...(await decodeJsonSuccess<T>(
        response,
        validator,
        validatorName,
        signal,
        metadataRequirements,
      )),
    };
  }

  return decodeProblemResponse(response, signal);
}

export async function decodeWriteResponse<T>(
  response: Response,
  validator: ProtocolValidator,
  validatorName: string,
  requirements: WriteResponseRequirements,
  signal?: AbortSignal,
): Promise<WriteResult<T>> {
  if (response.status === requirements.successStatus) {
    return decodeJsonSuccess(response, validator, validatorName, signal, requirements);
  }
  return decodeProblemResponse(response, signal);
}

async function decodeJsonSuccess<T>(
  response: Response,
  validator: ProtocolValidator,
  validatorName: string,
  signal: AbortSignal | undefined,
  requirements: ResponseMetadataRequirements,
): Promise<WriteResult<T>> {
  requireMediaType(response, "application/json", validatorName);
  const value = decodeProtocolValue<T>(
    await readJson(response, signal, validatorName),
    validator,
    validatorName,
  );
  return {
    value,
    metadata: readResponseMetadata(response, validatorName, requirements),
  };
}

async function decodeProblemResponse(
  response: Response,
  signal: AbortSignal | undefined,
): Promise<never> {
  requireMediaType(response, "application/problem+json", "validateProblem");
  const problem = decodeProtocolValue<ProtocolProblem>(
    await readJson(response, signal, "validateProblem"),
    validateProblem,
    "validateProblem",
  );
  if (problem.status !== response.status) {
    throw new ProtocolValidationError("validateProblem.status");
  }
  if (
    typeof problem.code !== "string" ||
    typeof problem.request_id !== "string" ||
    typeof problem.retryable !== "boolean" ||
    (problem.retry_after_seconds !== undefined &&
      (typeof problem.retry_after_seconds !== "number" ||
        !Number.isInteger(problem.retry_after_seconds)))
  ) {
    throw new ProtocolValidationError("validateProblem.safeFields");
  }

  try {
    throw new ProtocolHttpError({
      code: asProtocolErrorCode(problem.code),
      status: problem.status,
      requestId: asSafeRequestId(problem.request_id),
      retryable: problem.retryable,
      ...(problem.retry_after_seconds === undefined
        ? {}
        : { retryAfterSeconds: problem.retry_after_seconds }),
    });
  } catch (error) {
    if (error instanceof ProtocolHttpError) throw error;
    throw new ProtocolValidationError("validateProblem.safeFields");
  }
}

function readResponseMetadata(
  response: Response,
  validatorName: string,
  requirements: ResponseMetadataRequirements,
): ResponseMetadata {
  try {
    const etagHeader = response.headers.get("ETag");
    if (
      etagHeader === null &&
      (requirements.requireEntityTag === true || requirements.requireStrongEntityTag === true)
    ) {
      throw new ProtocolValidationError(`${validatorName}.etag`);
    }
    if (requirements.requireStrongEntityTag === true && etagHeader !== null) {
      if (!isStrongEntityTag(etagHeader)) {
        throw new ProtocolValidationError(`${validatorName}.etag`);
      }
    }
    const etag = etagHeader === null ? undefined : asEntityTag(etagHeader);

    const requestIdHeader = response.headers.get("X-Request-ID");
    const requestId = requestIdHeader === null ? undefined : asSafeRequestId(requestIdHeader);
    const protocolVersion = response.headers.get("Teslatlas-Protocol-Version") ?? undefined;
    if (protocolVersion !== undefined && !protocolVersionPattern.test(protocolVersion)) {
      throw new ProtocolValidationError(`${validatorName}.protocolVersion`);
    }

    const locationHeader = response.headers.get("Location");
    if (locationHeader === null && requirements.requireLocation === true) {
      throw new ProtocolValidationError(`${validatorName}.location`);
    }
    const location = locationHeader === null ? undefined : asSafeLocation(locationHeader);
    return {
      status: response.status,
      ...(etag === undefined ? {} : { etag }),
      ...(location === undefined ? {} : { location }),
      ...(requestId === undefined ? {} : { requestId }),
      ...(protocolVersion === undefined ? {} : { protocolVersion }),
    };
  } catch (error) {
    if (error instanceof ProtocolValidationError) throw error;
    throw new ProtocolValidationError(`${validatorName}.metadata`);
  }
}

function asSafeLocation(value: string): string {
  if (
    value.length === 0 ||
    containsControlCharacters(value) ||
    !value.startsWith("/") ||
    value.startsWith("//") ||
    value.includes("?") ||
    value.includes("#") ||
    /%(?![0-9a-f]{2})/iu.test(value)
  ) {
    throw new ProtocolValidationError("response.location");
  }
  try {
    const url = new URL(value, safeLocationBase);
    if (
      url.origin !== safeLocationBase ||
      url.username.length > 0 ||
      url.password.length > 0 ||
      url.search.length > 0 ||
      url.hash.length > 0
    ) {
      throw new ProtocolValidationError("response.location");
    }
  } catch (error) {
    if (error instanceof ProtocolValidationError) throw error;
    throw new ProtocolValidationError("response.location");
  }
  return value;
}

async function readJson(
  response: Response,
  signal: AbortSignal | undefined,
  validatorName: string,
): Promise<unknown> {
  try {
    return await response.json();
  } catch (error) {
    if (signal?.aborted === true) throw signal.reason ?? error;
    throw new ProtocolValidationError(validatorName);
  }
}

function requireMediaType(response: Response, expected: string, validatorName: string): void {
  const value = response.headers.get("Content-Type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (value !== expected) throw new ProtocolValidationError(`${validatorName}.contentType`);
}
