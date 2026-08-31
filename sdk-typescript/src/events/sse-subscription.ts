import type { MaybePromise } from "../auth/credential-store.js";
import { containsControlCharacters, TeslatlasError } from "../core/errors.js";
import type { FetchTransport } from "../http/fetch-transport.js";
import { parseSseStream, type SseEvent } from "./sse-parser.js";

export type { SseEvent } from "./sse-parser.js";

export interface SseCheckpointStore {
  load(): MaybePromise<string | undefined>;
  save(lastEventId: string | undefined): MaybePromise<void>;
}

export interface SseReconnectContext {
  readonly attempt: number;
  readonly reason: "eof" | "error";
  readonly error?: unknown;
  readonly serverRetryMilliseconds?: number;
}

export type SseReconnectPolicy = (context: SseReconnectContext) => MaybePromise<number | undefined>;

export type SseSleep = (milliseconds: number, signal: AbortSignal | undefined) => Promise<void>;

export interface SseSubscriptionOptions {
  readonly transport: FetchTransport;
  readonly path: string;
  readonly checkpoint?: SseCheckpointStore;
  readonly reconnect?: SseReconnectPolicy;
  readonly sleep?: SseSleep;
  readonly signal?: AbortSignal;
  readonly minimumServerRetryMilliseconds?: number;
  readonly maximumServerRetryMilliseconds?: number;
}

export class SseHttpError extends TeslatlasError<"sse_http_error"> {
  constructor(status: number) {
    super("Teslatlas event stream request failed", { code: "sse_http_error", status });
  }
}

export class SseContentTypeError extends TeslatlasError<"sse_content_type_error"> {
  constructor() {
    super("Teslatlas event stream response has an invalid content type", {
      code: "sse_content_type_error",
    });
  }
}

export class SseBodyError extends TeslatlasError<"sse_body_error"> {
  constructor() {
    super("Teslatlas event stream response has no readable body", {
      code: "sse_body_error",
    });
  }
}

export class InvalidSseCheckpointError extends TeslatlasError<"invalid_sse_checkpoint"> {
  constructor() {
    super("SSE checkpoint must contain no control characters and remain caller-owned", {
      code: "invalid_sse_checkpoint",
    });
  }
}

export class InvalidSseRetryConfigurationError extends TeslatlasError<"invalid_sse_retry_configuration"> {
  constructor() {
    super("SSE retry delays must be nonnegative safe integers with valid bounds", {
      code: "invalid_sse_retry_configuration",
    });
  }
}

export async function* subscribeToSse(options: SseSubscriptionOptions): AsyncIterable<SseEvent> {
  const minimumServerRetryMilliseconds = options.minimumServerRetryMilliseconds ?? 0;
  const maximumServerRetryMilliseconds = options.maximumServerRetryMilliseconds ?? 30_000;
  assertRetryBounds(minimumServerRetryMilliseconds, maximumServerRetryMilliseconds);

  let committedLastEventId = await loadCheckpoint(options.checkpoint);
  validateCheckpoint(committedLastEventId);
  let reconnectAttempt = 1;

  while (true) {
    throwIfAborted(options.signal);
    let serverRetryMilliseconds: number | undefined;
    let reconnectReason: "eof" | "error" = "eof";
    let reconnectError: unknown;

    try {
      const headers = new Headers({ Accept: "text/event-stream" });
      if (committedLastEventId !== undefined && committedLastEventId.length > 0) {
        headers.set("Last-Event-ID", committedLastEventId);
      }

      const response = await options.transport.request(options.path, {
        method: "GET",
        headers,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
      throwIfAborted(options.signal);

      if (!response.ok) {
        throw new SseHttpError(response.status);
      }
      if (!isEventStreamContentType(response.headers.get("Content-Type"))) {
        throw new SseContentTypeError();
      }
      if (response.body === null) {
        throw new SseBodyError();
      }

      for await (const item of parseSseStream(response.body, {
        ...(committedLastEventId === undefined ? {} : { initialLastEventId: committedLastEventId }),
      })) {
        throwIfAborted(options.signal);
        if (item.type === "retry") {
          serverRetryMilliseconds = item.milliseconds;
          continue;
        }
        if (item.type === "checkpoint") {
          committedLastEventId = normalizeCheckpoint(item.lastEventId);
          await saveCheckpoint(options.checkpoint, committedLastEventId);
          continue;
        }

        yield {
          event: item.event,
          data: item.data,
          lastEventId: item.lastEventId,
        };
        throwIfAborted(options.signal);
        committedLastEventId = normalizeCheckpoint(item.lastEventId);
        await saveCheckpoint(options.checkpoint, committedLastEventId);
      }
    } catch (error) {
      if (options.signal?.aborted === true) {
        throw abortReason(options.signal);
      }
      if (error instanceof InvalidSseCheckpointError) {
        throw error;
      }
      reconnectReason = "error";
      reconnectError = error;
    }

    const policyDelay = await decideReconnectDelay(options.reconnect, {
      attempt: reconnectAttempt,
      reason: reconnectReason,
      ...(reconnectError === undefined ? {} : { error: reconnectError }),
      ...(serverRetryMilliseconds === undefined ? {} : { serverRetryMilliseconds }),
    });
    throwIfAborted(options.signal);

    if (policyDelay === undefined) {
      if (reconnectReason === "error") {
        throw reconnectError;
      }
      return;
    }

    const delay =
      serverRetryMilliseconds === undefined
        ? policyDelay
        : clamp(
            serverRetryMilliseconds,
            minimumServerRetryMilliseconds,
            maximumServerRetryMilliseconds,
          );
    const sleep = options.sleep ?? defaultSleep;
    await sleep(delay, options.signal);
    throwIfAborted(options.signal);
    reconnectAttempt += 1;
  }
}

async function loadCheckpoint(store: SseCheckpointStore | undefined): Promise<string | undefined> {
  if (store === undefined) {
    return undefined;
  }
  try {
    return await store.load();
  } catch {
    throw new InvalidSseCheckpointError();
  }
}

async function saveCheckpoint(
  store: SseCheckpointStore | undefined,
  value: string | undefined,
): Promise<void> {
  validateCheckpoint(value);
  if (store === undefined) {
    return;
  }
  try {
    await store.save(value);
  } catch {
    throw new InvalidSseCheckpointError();
  }
}

function normalizeCheckpoint(value: string): string | undefined {
  validateCheckpoint(value);
  return value.length === 0 ? undefined : value;
}

function validateCheckpoint(value: unknown): asserts value is string | undefined {
  if (value !== undefined && (typeof value !== "string" || containsControlCharacters(value))) {
    throw new InvalidSseCheckpointError();
  }
}

function isEventStreamContentType(value: string | null): boolean {
  if (value === null) {
    return false;
  }
  const essence = value.split(";", 1)[0]?.trim().toLowerCase();
  return essence === "text/event-stream";
}

async function decideReconnectDelay(
  policy: SseReconnectPolicy | undefined,
  context: SseReconnectContext,
): Promise<number | undefined> {
  if (policy === undefined) {
    return undefined;
  }
  const delay = await policy(context);
  if (delay !== undefined && !isNonNegativeSafeInteger(delay)) {
    throw new InvalidSseRetryConfigurationError();
  }
  return delay;
}

function assertRetryBounds(minimum: number, maximum: number): void {
  if (
    !isNonNegativeSafeInteger(minimum) ||
    !isNonNegativeSafeInteger(maximum) ||
    minimum > maximum
  ) {
    throw new InvalidSseRetryConfigurationError();
  }
}

function isNonNegativeSafeInteger(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}

async function defaultSleep(milliseconds: number, signal: AbortSignal | undefined): Promise<void> {
  throwIfAborted(signal);
  if (milliseconds === 0) {
    return;
  }

  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, milliseconds);
    const onAbort = () => {
      clearTimeout(timeout);
      reject(
        signal === undefined ? new DOMException("Aborted", "AbortError") : abortReason(signal),
      );
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted === true) {
    throw abortReason(signal);
  }
}

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException("Aborted", "AbortError");
}
