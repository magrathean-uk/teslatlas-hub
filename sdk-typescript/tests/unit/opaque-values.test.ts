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

  it("preserves weak entity tags without interpreting them", () => {
    const value = 'W/"revision-7"';

    expect(asEntityTag(value)).toBe(value);
  });

  it("rejects empty or header-injecting entity tags", () => {
    expect(() => asEntityTag("")).toThrow(InvalidEntityTagError);
    expect(() => asEntityTag('"revision-7"\r\nAuthorization: secret')).toThrow(
      InvalidEntityTagError,
    );
  });
});
