import { describe, expect, it } from "vitest";
import {
  InvalidCompatibilityWindowError,
  InvalidProtocolVersionError,
  checkProtocolVersion,
  parseProtocolVersion,
} from "../../src/core/version.js";

describe("protocol semantic versions", () => {
  it("parses a complete semantic version without changing identifiers", () => {
    expect(parseProtocolVersion("1.4.2-alpha.1+build.7")).toEqual({
      major: 1,
      minor: 4,
      patch: 2,
      prerelease: "alpha.1",
      build: "build.7",
    });
  });

  it.each(["1", "1.2", "01.2.3", "1.02.3", "1.2.03", "1.2.3-01", "1.2.3+"])(
    "rejects invalid semantic version %s",
    (value) => {
      expect(() => parseProtocolVersion(value)).toThrow(InvalidProtocolVersionError);
    },
  );

  it("checks compatibility against a caller-supplied minor window", () => {
    const window = { major: 1, minimumMinor: 3, maximumMinor: 5 };

    expect(checkProtocolVersion(parseProtocolVersion("1.4.0"), window)).toEqual({
      compatible: true,
    });
    expect(checkProtocolVersion(parseProtocolVersion("2.4.0"), window)).toEqual({
      compatible: false,
      reason: "unsupported-major",
    });
    expect(checkProtocolVersion(parseProtocolVersion("1.2.9"), window)).toEqual({
      compatible: false,
      reason: "below-minimum-minor",
    });
    expect(checkProtocolVersion(parseProtocolVersion("1.6.0"), window)).toEqual({
      compatible: false,
      reason: "above-maximum-minor",
    });
  });

  it("rejects a reversed compatibility window", () => {
    expect(() =>
      checkProtocolVersion(parseProtocolVersion("1.4.0"), {
        major: 1,
        minimumMinor: 5,
        maximumMinor: 4,
      }),
    ).toThrow(InvalidCompatibilityWindowError);
  });
});
