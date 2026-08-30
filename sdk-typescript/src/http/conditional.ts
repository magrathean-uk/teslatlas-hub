import { containsControlCharacters, TeslatlasError } from "../core/errors.js";
import { asEntityTag, type EntityTag, type OpaqueCursor } from "../core/opaque-values.js";

export class InvalidQueryParameterNameError extends TeslatlasError<"invalid_query_parameter_name"> {
  constructor() {
    super("Query parameter name must be non-empty and contain no control characters", {
      code: "invalid_query_parameter_name",
    });
  }
}

export function appendOpaqueQueryValue(url: URL, parameterName: string, value: OpaqueCursor): URL {
  if (parameterName.length === 0 || containsControlCharacters(parameterName)) {
    throw new InvalidQueryParameterNameError();
  }
  const result = new URL(url.href);
  result.searchParams.set(parameterName, value);
  return result;
}

export function applyIfNoneMatch(headers: HeadersInit, entityTag: EntityTag): Headers {
  const result = new Headers(headers);
  result.set("If-None-Match", entityTag);
  return result;
}

export function readEntityTag(headers: Headers): EntityTag | undefined {
  const value = headers.get("ETag");
  return value === null ? undefined : asEntityTag(value);
}

export function isNotModified(status: number): boolean {
  return status === 304;
}
