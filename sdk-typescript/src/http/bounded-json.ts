import { ProtocolValidationError } from "../core/errors.js";

// Match the rich Swift transport's bound; the current-Hub binding keeps its
// separate 1 MiB limit. Count received bytes, never trust Content-Length.
export const maximumJsonResponseBytes = 16 * 1_024 * 1_024;

export async function readBoundedText(
  response: Response,
  signal: AbortSignal | undefined,
  validatorName: string,
  maximumBytes: number,
): Promise<string> {
  if (response.body === null) {
    throwIfAborted(signal);
    return "";
  }
  const reader = response.body.getReader();
  let body = new Uint8Array(Math.min(4_096, maximumBytes));
  let size = 0;
  let emptyChunks = 0;
  let completed = false;
  // One listener interrupts the pending read. Repeated Promise.race calls against
  // one unresolved abort promise would retain a reaction for every transport chunk.
  const abortListener = () => {
    void reader.cancel().catch(() => undefined);
  };
  signal?.addEventListener("abort", abortListener, { once: true });
  try {
    throwIfAborted(signal);
    while (true) {
      const result = await reader.read();
      if (signal?.aborted === true)
        throw signal.reason ?? new DOMException("Aborted", "AbortError");
      if (result.done) {
        completed = true;
        break;
      }
      if (result.value.byteLength > maximumBytes - size) {
        throw new ProtocolValidationError(`${validatorName}.size`);
      }
      emptyChunks = result.value.byteLength === 0 ? emptyChunks + 1 : 0;
      if (emptyChunks > 1_024) throw new ProtocolValidationError(`${validatorName}.size`);
      const needed = size + result.value.byteLength;
      if (needed > body.byteLength) {
        const expanded = new Uint8Array(
          Math.min(maximumBytes, Math.max(needed, body.byteLength * 2)),
        );
        expanded.set(body.subarray(0, size));
        body = expanded;
      }
      body.set(result.value, size);
      size += result.value.byteLength;
    }
    return new TextDecoder().decode(body.subarray(0, size));
  } catch (error) {
    if (signal?.aborted === true) throw signal.reason ?? error;
    if (error instanceof ProtocolValidationError) throw error;
    throw new ProtocolValidationError(validatorName);
  } finally {
    signal?.removeEventListener("abort", abortListener);
    if (!completed) void reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

export async function readBoundedJson(
  response: Response,
  signal: AbortSignal | undefined,
  validatorName: string,
): Promise<unknown> {
  const body = await readBoundedText(response, signal, validatorName, maximumJsonResponseBytes);
  try {
    return parseProtocolJson(body, validatorName);
  } catch {
    throw new ProtocolValidationError(validatorName);
  }
}

function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted === true) throw signal.reason ?? new DOMException("Aborted", "AbortError");
}

// Parsed objects carry their effective numeric lexemes, without changing public
// JSON values. The mirror uses JSON.parse's duplicate-final-member semantics.
const numericLexemes = new WeakMap<object, Map<string | number, string>>();

export function parseProtocolJson(source: string, validatorName: string): unknown {
  try {
    const mirror = JSON.parse(numericMirror(source));
    const value: unknown = JSON.parse(source);
    visitJson(value, mirror, (parent, key, current, lexical) => {
      if (!Number.isFinite(current)) throw new ProtocolValidationError(`${validatorName}.number`);
      if (parent === undefined || key === undefined) return;
      let tokens = numericLexemes.get(parent);
      if (tokens === undefined) {
        tokens = new Map();
        numericLexemes.set(parent, tokens);
      }
      tokens.set(key, lexical as string);
    });
    return value;
  } catch (error) {
    if (error instanceof ProtocolValidationError) throw error;
    throw new ProtocolValidationError(validatorName);
  }
}

function numericMirror(source: string): string {
  const token = /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/uy;
  const pieces: string[] = [];
  let copied = 0;
  for (let index = 0; index < source.length; ) {
    if (source[index] === '"') {
      index += 1;
      while (index < source.length) {
        const quote = source.indexOf('"', index);
        if (quote < 0) {
          index = source.length;
          break;
        }
        let slash = quote - 1;
        while (source[slash] === "\\") slash -= 1;
        index = quote + 1;
        if ((quote - slash - 1) % 2 === 0) break;
      }
      continue;
    }
    token.lastIndex = index;
    const match = token.exec(source);
    if (match === null) {
      index += 1;
      continue;
    }
    pieces.push(source.slice(copied, index), JSON.stringify(match[0]));
    index = token.lastIndex;
    copied = index;
  }
  pieces.push(source.slice(copied));
  return pieces.join("");
}

/** @internal Used by generated schema integer checks, including nested records. */
export function isExactJsonInteger(value: number, parent: unknown, key: string | number): boolean {
  if (!Number.isFinite(value) || !Number.isInteger(value)) return false;
  const token =
    parent !== null && typeof parent === "object"
      ? numericLexemes.get(parent)?.get(String(key))
      : undefined;
  if (token === undefined) return true; // Already-constructed Numbers retain the schema domain.
  const parts = /^(-?)([0-9]+)(?:\.([0-9]+))?(?:[eE]([+-]?[0-9]+))?$/u.exec(token);
  if (parts === null) return false;
  const fraction = parts[3] ?? "";
  const digits = ((parts[2] ?? "") + fraction).replace(/^0+/u, "");
  if (digits.length === 0) return true;
  const significant = digits.replace(/0+$/u, "");
  const scale = Number(parts[4] ?? "0") - fraction.length + digits.length - significant.length;
  if (scale < 0 || !Number.isFinite(scale) || significant.length + scale > 309) return false;
  return BigInt((parts[1] ?? "") + significant + "0".repeat(scale)) === BigInt(value);
}

export function assertFiniteNumbers(value: unknown, validatorName: string): void {
  visitJson(value, undefined, (_parent, _key, current) => {
    if (!Number.isFinite(current)) throw new ProtocolValidationError(`${validatorName}.number`);
  });
}

// Walk one child at a time. A wide byte-bounded page must not create a second
// frontier array of every item or overflow argument limits with array spread.
function visitJson(
  value: unknown,
  mirror: unknown,
  number: (
    parent: object | undefined,
    key: string | undefined,
    value: number,
    lexical: unknown,
  ) => void,
): void {
  interface Frame {
    value: object;
    mirror: unknown;
    keys?: string[];
    length: number;
    index: number;
  }
  const frames: Frame[] = [];
  const visited = new WeakSet<object>();
  const visit = (child: unknown, lexical: unknown, parent?: object, key?: string) => {
    if (typeof child === "number") number(parent, key, child, lexical);
    if (child === null || typeof child !== "object" || visited.has(child)) return;
    visited.add(child);
    const keys = Array.isArray(child) ? undefined : Object.keys(child);
    frames.push({
      value: child,
      mirror: lexical,
      ...(keys === undefined ? {} : { keys }),
      length: keys?.length ?? (child as unknown[]).length,
      index: 0,
    });
  };
  visit(value, mirror);
  while (frames.length > 0) {
    const frame = frames[frames.length - 1];
    if (frame === undefined) break;
    if (frame.index === frame.length) {
      visited.delete(frame.value);
      frames.pop();
      continue;
    }
    const key = frame.keys?.[frame.index] ?? String(frame.index);
    frame.index += 1;
    visit(
      (frame.value as Record<string, unknown>)[key],
      frame.mirror === undefined ? undefined : (frame.mirror as Record<string, unknown>)[key],
      frame.value,
      key,
    );
  }
}
