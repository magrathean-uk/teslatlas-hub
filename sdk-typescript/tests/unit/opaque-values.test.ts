import { describe, expect, it } from "vitest";
import {
  InvalidEntityTagError,
  InvalidOpaqueCursorError,
  asEntityTag,
  asOpaqueCursor,
} from "../../src/core/opaque-values.js";

describe("opaque protocol values", () => {
  it("preserves cursor bytes represented by the caller string", () => {
    const value = "opaque+/= cursor";

    expect(asOpaqueCursor(value)).toBe(value);
  });

  it("rejects empty or control-character cursor values", () => {
    expect(() => asOpaqueCursor("")).toThrow(InvalidOpaqueCursorError);
    expect(() => asOpaqueCursor("cursor\r\nAuthorization: secret")).toThrow(
      InvalidOpaqueCursorError,
    );
  });

  it("preserves protocol-valid opaque weak entity tags without interpreting them", () => {
    const value = 'W/"opaque+/= revision-7"';

    expect(asEntityTag(value)).toBe(value);
  });

  it("accepts a protocol-valid response ETag beyond the request header limit", () => {
    const value = `"${"x".repeat(512)}"`;

    expect(asEntityTag(value)).toBe(value);
  });

  it.each(["", "not-an-etag", "W/not-quoted", 'W/""', '"revision-7"\r\nAuthorization: secret'])(
    "rejects malformed entity tag %j",
    (value) => {
      expect(() => asEntityTag(value)).toThrow(InvalidEntityTagError);
    },
  );
});
