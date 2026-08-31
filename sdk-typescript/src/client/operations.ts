import { ProtocolValidationError, TeslatlasError, type SafeRequestId } from "../core/errors.js";
import type { EntityTag, OpaqueCursor } from "../core/opaque-values.js";

export interface ResponseMetadata {
  readonly status: number;
  readonly etag?: EntityTag;
  readonly location?: string;
  readonly requestId?: SafeRequestId;
  readonly protocolVersion?: string;
}

export type ReadResult<T> =
  | { readonly kind: "modified"; readonly value: T; readonly metadata: ResponseMetadata }
  | { readonly kind: "not-modified"; readonly metadata: ResponseMetadata };

export interface WriteResult<T> {
  readonly value: T;
  readonly metadata: ResponseMetadata;
}

export interface RequestOptions {
  readonly signal?: AbortSignal;
}

export interface ConditionalReadOptions extends RequestOptions {
  readonly ifNoneMatch?: EntityTag;
}

export interface PageReadOptions extends ConditionalReadOptions {
  readonly cursor?: OpaqueCursor;
  readonly limit?: number;
}

export interface HistoryPageOptions extends PageReadOptions {
  readonly from?: string;
  readonly to?: string;
}

export interface DataQualityPageOptions extends HistoryPageOptions {
  readonly vehicleId?: string;
}

export class InvalidReadOptionsError extends TeslatlasError<"invalid_read_options"> {
  constructor() {
    super("Teslatlas read options are invalid", { code: "invalid_read_options" });
  }
}

interface CursorPage {
  readonly next_cursor: string | null;
}

export async function* iteratePages<
  TPage extends CursorPage,
  TOptions extends { readonly cursor?: OpaqueCursor },
>(
  load: (options: TOptions) => Promise<ReadResult<TPage>>,
  firstOptions: TOptions,
): AsyncGenerator<TPage, void, undefined> {
  let options = firstOptions;
  const seen = new Set<string>();
  if (options.cursor !== undefined) seen.add(options.cursor);

  while (true) {
    const result = await load(options);
    if (result.kind === "not-modified") return;
    yield result.value;

    const cursor = result.value.next_cursor;
    if (cursor === null) return;
    if (seen.has(cursor)) {
      throw new ProtocolValidationError("pagination.next_cursor");
    }
    seen.add(cursor);
    options = { ...options, cursor: cursor as OpaqueCursor };
  }
}
