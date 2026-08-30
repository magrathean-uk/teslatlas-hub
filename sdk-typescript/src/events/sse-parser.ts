export interface SseEvent {
  readonly event: string;
  readonly data: string;
  readonly lastEventId: string;
}

export type SseStreamItem =
  | ({ readonly type: "event" } & SseEvent)
  | { readonly type: "retry"; readonly milliseconds: number }
  | { readonly type: "checkpoint"; readonly lastEventId: string };

export interface ParseSseStreamOptions {
  readonly initialLastEventId?: string;
}

export async function* parseSseStream(
  stream: ReadableStream<Uint8Array>,
  options: ParseSseStreamOptions = {},
): AsyncIterable<SseStreamItem> {
  const parser = new IncrementalSseParser(options.initialLastEventId ?? "");
  const decoder = new TextDecoder();
  const reader = stream.getReader();
  let completed = false;

  try {
    while (true) {
      const result = await reader.read();
      if (result.done) {
        completed = true;
        break;
      }
      parser.feed(decoder.decode(result.value, { stream: true }));
      yield* parser.takeItems();
    }

    parser.feed(decoder.decode());
    yield* parser.takeItems();
  } finally {
    if (!completed) {
      await reader.cancel();
    }
    reader.releaseLock();
  }
}

class IncrementalSseParser {
  readonly #items: SseStreamItem[] = [];
  readonly #dataLines: string[] = [];
  #line = "";
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

    for (const character of value) {
      if (this.#skipLineFeed) {
        this.#skipLineFeed = false;
        if (character === "\n") {
          continue;
        }
      }

      if (character === "\r") {
        this.#handleLine(this.#line);
        this.#line = "";
        this.#skipLineFeed = true;
      } else if (character === "\n") {
        this.#handleLine(this.#line);
        this.#line = "";
      } else {
        this.#line += character;
      }
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
        this.#dataLines.push(value);
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

  #dispatchBlock(): void {
    if (this.#dataLines.length > 0) {
      this.#items.push({
        type: "event",
        event: this.#eventType.length === 0 ? "message" : this.#eventType,
        data: this.#dataLines.join("\n"),
        lastEventId: this.#lastEventId,
      });
    } else if (this.#blockIdChanged) {
      this.#items.push({ type: "checkpoint", lastEventId: this.#lastEventId });
    }

    this.#dataLines.length = 0;
    this.#eventType = "";
    this.#blockIdChanged = false;
  }
}
