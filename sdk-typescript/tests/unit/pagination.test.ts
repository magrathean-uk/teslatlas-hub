import { describe, expect, it } from "vitest";
import type { OpaqueCursor } from "../../src/core/opaque-values.js";
import { asOpaqueCursor } from "../../src/core/opaque-values.js";
import { iteratePages, type ReadResult } from "../../src/client/operations.js";

interface Page {
  readonly items: readonly number[];
  readonly next_cursor: string | null;
}

describe("page iterator", () => {
  it("yields modified pages and forwards next_cursor bytes unchanged", async () => {
    const observed: Array<OpaqueCursor | undefined> = [];
    const pages: Page[] = [
      { items: [1], next_cursor: "opaque_cursor_0002" },
      { items: [2], next_cursor: null },
    ];

    const yielded: Page[] = [];
    for await (const page of iteratePages(
      async (options: { cursor?: OpaqueCursor }) => {
        observed.push(options.cursor);
        const value = pages.shift();
        if (value === undefined) throw new Error("unexpected load");
        return modified(value);
      },
      { cursor: asOpaqueCursor("opaque_cursor_0001") },
    )) {
      yielded.push(page);
    }

    expect(observed).toEqual(["opaque_cursor_0001", "opaque_cursor_0002"]);
    expect(yielded.map(({ items }) => items)).toEqual([[1], [2]]);
  });

  it("stops on not-modified without yielding or retrying", async () => {
    let calls = 0;
    const yielded = [];
    for await (const page of iteratePages(async () => {
      calls += 1;
      return { kind: "not-modified", metadata: { status: 304 } } as const;
    }, {})) {
      yielded.push(page);
    }

    expect(yielded).toEqual([]);
    expect(calls).toBe(1);
  });

  it("rejects a repeated cursor before loading it again", async () => {
    let calls = 0;
    const consume = async () => {
      for await (const _page of iteratePages(async () => {
        calls += 1;
        return modified({ items: [calls], next_cursor: "opaque_cursor_repeat" });
      }, {})) {
        // consume
      }
    };

    await expect(consume()).rejects.toMatchObject({
      code: "protocol_validation",
      validator: "pagination.next_cursor",
    });
    expect(calls).toBe(2);
  });
});

function modified<T>(value: T): ReadResult<T> {
  return { kind: "modified", value, metadata: { status: 200 } };
}
