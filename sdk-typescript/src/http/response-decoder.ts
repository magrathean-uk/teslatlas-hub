import {
  asProtocolErrorCode,
  asSafeRequestId,
  containsControlCharacters,
  ProtocolHttpError,
  ProtocolValidationError,
} from "../core/errors.js";
import { asEntityTag } from "../core/opaque-values.js";
import { validateProblem } from "../generated/validators.js";
import type { ReadResult, ResponseMetadata } from "../client/operations.js";
import type { ProtocolProblem } from "../protocol/models.js";
import { decodeProtocolValue, type ProtocolValidator } from "../protocol/validate.js";

const protocolVersionPattern = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u;

export async function decodeReadResponse<T>(
  response: Response,
  validator: ProtocolValidator,
  validatorName: string,
  signal?: AbortSignal,
): Promise<ReadResult<T>> {
  if (response.status === 304) {
    if (response.body !== null) throw new ProtocolValidationError(`${validatorName}.304`);
    const metadata = readResponseMetadata(response, validatorName, true);
    return { kind: "not-modified", metadata };
  }

  if (response.status === 200) {
    requireMediaType(response, "application/json", validatorName);
    const value = decodeProtocolValue<T>(
      await readJson(response, signal, validatorName),
      validator,
      validatorName,
    );
    return {
      kind: "modified",
      value,
      metadata: readResponseMetadata(response, validatorName, false),
    };
  }

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
  requireEntityTag: boolean,
): ResponseMetadata {
  try {
    const etagHeader = response.headers.get("ETag");
    if (requireEntityTag && etagHeader === null) {
      throw new ProtocolValidationError(`${validatorName}.etag`);
    }
    const etag = etagHeader === null ? undefined : asEntityTag(etagHeader);
    const requestIdHeader = response.headers.get("X-Request-ID");
    const requestId = requestIdHeader === null ? undefined : asSafeRequestId(requestIdHeader);
    const protocolVersion = response.headers.get("Teslatlas-Protocol-Version") ?? undefined;
    if (protocolVersion !== undefined && !protocolVersionPattern.test(protocolVersion)) {
      throw new ProtocolValidationError(`${validatorName}.protocolVersion`);
    }
    const location = response.headers.get("Location") ?? undefined;
    if (location !== undefined && containsControlCharacters(location)) {
      throw new ProtocolValidationError(`${validatorName}.location`);
    }
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
