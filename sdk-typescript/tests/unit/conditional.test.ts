import { describe, expect, it } from "vitest";
import { asEntityTag, asOpaqueCursor } from "../../src/core/opaque-values.js";
import {
  appendOpaqueQueryValue,
  applyIfNoneMatch,
  isNotModified,
  readEntityTag,
} from "../../src/http/conditional.js";

describe("conditional HTTP helpers", () => {
  it("round-trips an opaque cursor through URL encoding", () => {
    const original = new URL("https://hub.example/v1/items?limit=20");
    const cursor = asOpaqueCursor("opaque+/= cursor");
    const result = appendOpaqueQueryValue(original, "cursor", cursor);

    expect(result).not.toBe(original);
    expect(result.searchParams.get("limit")).toBe("20");
    expect(result.searchParams.get("cursor")).toBe(cursor);
  });

  it("applies If-None-Match without mutating caller headers", () => {
    const original = new Headers({ Accept: "application/json" });
    const result = applyIfNoneMatch(original, asEntityTag('W/"revision-7"'));

    expect(original.has("if-none-match")).toBe(false);
    expect(result.get("if-none-match")).toBe('W/"revision-7"');
  });

  it("reads an ETag and recognizes only status 304 as not modified", () => {
    const headers = new Headers({ ETag: '"revision-8"' });

    expect(readEntityTag(headers)).toBe(asEntityTag('"revision-8"'));
    expect(isNotModified(304)).toBe(true);
    expect(isNotModified(200)).toBe(false);
  });

  it("rejects an empty or header-like query parameter name", () => {
    const url = new URL("https://hub.example/v1/items");
    const cursor = asOpaqueCursor("next");

    expect(() => appendOpaqueQueryValue(url, "", cursor)).toThrow();
    expect(() => appendOpaqueQueryValue(url, "cursor\nadmin", cursor)).toThrow();
  });
});
