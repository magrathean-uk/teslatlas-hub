import { ProtocolValidationError } from "../core/errors.js";

const encoder = new TextEncoder();
const maximumEventDataBytes = 8 * 1_024 * 1_024;
// A single data line can use the event budget plus its "data: " framing.
// Aggregate data accounting below still includes each appended newline.
const maximumLineBytes = maximumEventDataBytes + 6;

/** @internal Resource violations are terminal, rather than reconnectable IO. */
export class SseLimitError extends ProtocolValidationError {}

export interface SseEvent {
  readonly event: string;
  readonly data: string;
  readonly lastEventId: string;
}

/** @internal */
export class SseStreamReadError extends Error {
  constructor() {
    super("SSE stream read failed");
    this.name = "SseStreamReadError";
  }
}

export type SseStreamItem =
  | ({ readonly type: "event" } & SseEvent)
  | { readonly type: "retry"; readonly milliseconds: number }
  | { readonly type: "checkpoint"; readonly lastEventId: string };

export interface ParseSseStreamOptions {
  readonly initialLastEventId?: string;
  readonly signal?: AbortSignal;
}

export async function* parseSseStream(
  stream: ReadableStream<Uint8Array>,
  options: ParseSseStreamOptions = {},
): AsyncIterable<SseStreamItem> {
  const parser = new IncrementalSseParser(options.initialLastEventId ?? "");
  // Preserve the BOM for the parser to strip exactly one leading U+FEFF.
  const decoder = new TextDecoder("utf-8", { ignoreBOM: true });
  let reader: ReadableStreamDefaultReader<Uint8Array>;
  try {
    reader = stream.getReader();
  } catch {
    throw new SseStreamReadError();
  }
  let completed = false;
  const throwIfAborted = () => {
    if (options.signal?.aborted === true) {
      throw options.signal.reason ?? new DOMException("Aborted", "AbortError");
    }
  };
  const onAbort = () => {
    void reader.cancel().catch(() => {});
  };
  options.signal?.addEventListener("abort", onAbort, { once: true });

  try {
    throwIfAborted();
    while (true) {
      let result: ReadableStreamReadResult<Uint8Array>;
      try {
        result = await reader.read();
        throwIfAborted();
      } catch {
        throwIfAborted();
        throw new SseStreamReadError();
      }
      if (result.done) {
        completed = true;
        break;
      }
      // Feed bounded windows so a single transport chunk cannot queue an
      // arbitrary number of complete events before backpressure reaches it.
      for (let offset = 0; offset < result.value.byteLength; offset += 4_096) {
        parser.feed(
          decoder.decode(result.value.subarray(offset, offset + 4_096), { stream: true }),
        );
        for (const item of parser.takeItems()) {
          throwIfAborted();
          yield item;
        }
      }
    }

    parser.feed(decoder.decode());
    for (const item of parser.takeItems()) {
      throwIfAborted();
      yield item;
    }
  } finally {
    options.signal?.removeEventListener("abort", onAbort);
    if (!completed) {
      try {
        // Initiate cleanup without making consumer return wait on upstream code.
        void reader.cancel().catch(() => {});
      } catch {}
    }
    reader.releaseLock();
  }
}

class IncrementalSseParser {
  readonly #items: SseStreamItem[] = [];
  readonly #dataFragments: string[] = [];
  readonly #lineFragments: string[] = [];
  #pendingData = "";
  #pendingLine = "";
  #emptyDataLines = 0;
  #lineBytes = 0;
  #eventDataBytes = 0;
  #skipLineFeed = false;
  #atStart = true;
  #eventType = "";
  #lastEventId: string;
  #blockIdChanged = false;

  constructor(initialLastEventId: string) {
    this.#lastEventId = initialLastEventId;
  }

  feed(decoded: string): void {
    let value = decoded;
    if (this.#atStart && value.length > 0) {
      this.#atStart = false;
      if (value.startsWith("\uFEFF")) {
        value = value.slice(1);
      }
    }

    const delimiter = /[\r\n]/gu;
    let offset = 0;
    while (offset < value.length) {
      if (this.#skipLineFeed) {
        this.#skipLineFeed = false;
        if (value[offset] === "\n") {
          offset += 1;
          continue;
        }
      }
      delimiter.lastIndex = offset;
      const end = delimiter.exec(value)?.index ?? value.length;
      const fragment = value.slice(offset, end);
      this.#lineBytes += fragment.length === 0 ? 0 : encoder.encode(fragment).byteLength;
      if (this.#lineBytes > maximumLineBytes) throw new SseLimitError("Sse.lineSize");
      this.#appendLine(fragment);
      if (end === value.length) break;
      this.#handleLine(this.#lineFragments.join("") + this.#pendingLine);
      this.#lineFragments.length = 0;
      this.#pendingLine = "";
      this.#lineBytes = 0;
      this.#skipLineFeed = value[end] === "\r";
      offset = end + 1;
    }
  }

  *takeItems(): Iterable<SseStreamItem> {
    while (this.#items.length > 0) {
      const item = this.#items.shift();
      if (item !== undefined) {
        yield item;
      }
    }
  }

  #handleLine(line: string): void {
    if (line.length === 0) {
      this.#dispatchBlock();
      return;
    }
    if (line.startsWith(":")) {
      return;
    }

    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    let value = colon === -1 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) {
      value = value.slice(1);
    }

    switch (field) {
      case "data":
        this.#eventDataBytes += (value.length === 0 ? 0 : encoder.encode(value).byteLength) + 1;
        if (this.#eventDataBytes > maximumEventDataBytes) throw new SseLimitError("Sse.eventSize");
        if (value.length === 0) this.#emptyDataLines += 1;
        else {
          this.#flushEmptyData();
          this.#appendData(`${value}\n`);
        }
        break;
      case "event":
        this.#eventType = value;
        break;
      case "id":
        if (!value.includes("\0")) {
          this.#lastEventId = value;
          this.#blockIdChanged = true;
        }
        break;
      case "retry": {
        if (/^\d+$/u.test(value)) {
          const milliseconds = Number(value);
          if (Number.isSafeInteger(milliseconds)) {
            this.#items.push({ type: "retry", milliseconds });
          }
        }
        break;
      }
      default:
        break;
    }
  }

  #appendLine(value: string): void {
    // Small transport chunks cannot create an entry for every byte of a line.
    for (let offset = 0; offset < value.length; ) {
      const take = Math.min(4096 - this.#pendingLine.length, value.length - offset);
      this.#pendingLine += value.slice(offset, offset + take);
      offset += take;
      if (this.#pendingLine.length === 4096) {
        this.#lineFragments.push(this.#pendingLine);
        this.#pendingLine = "";
      }
    }
  }

  #appendData(value: string): void {
    // Bounded segments avoid per-character line ropes and per-empty-line arrays.
    for (let offset = 0; offset < value.length; ) {
      const take = Math.min(4096 - this.#pendingData.length, value.length - offset);
      this.#pendingData += value.slice(offset, offset + take);
      offset += take;
      if (this.#pendingData.length === 4096) {
        this.#dataFragments.push(this.#pendingData);
        this.#pendingData = "";
      }
    }
  }

  #flushEmptyData(): void {
    if (this.#emptyDataLines > 0) {
      this.#appendData("\n".repeat(this.#emptyDataLines));
      this.#emptyDataLines = 0;
    }
  }

  #dispatchBlock(): void {
    if (this.#eventDataBytes > 0) {
      this.#flushEmptyData();
      this.#items.push({
        type: "event",
        event: this.#eventType.length === 0 ? "message" : this.#eventType,
        data: (this.#dataFragments.join("") + this.#pendingData).slice(0, -1),
        lastEventId: this.#lastEventId,
      });
    } else if (this.#blockIdChanged) {
      this.#items.push({ type: "checkpoint", lastEventId: this.#lastEventId });
    }

    this.#dataFragments.length = 0;
    this.#pendingData = "";
    this.#emptyDataLines = 0;
    this.#eventDataBytes = 0;
    this.#eventType = "";
    this.#blockIdChanged = false;
  }
}
