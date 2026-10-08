import { describe, expect, it } from "vitest";
import {
  InvalidCapabilityError,
  MissingCapabilitiesError,
  asCapabilityId,
  createCapabilitySet,
  findMissingCapabilities,
  requireCapabilities,
} from "../../src/core/capabilities.js";

describe("capability sets", () => {
  it("deduplicates exact opaque capability identifiers", () => {
    const capabilities = createCapabilitySet(["events", "query", "events"]);

    expect([...capabilities]).toEqual([asCapabilityId("events"), asCapabilityId("query")]);
  });

  it("does not case-fold capability identifiers", () => {
    const capabilities = createCapabilitySet(["events", "Events"]);

    expect(capabilities.size).toBe(2);
  });

  it("reports missing capabilities in deterministic lexical order", () => {
    const available = createCapabilitySet(["events"]);

    expect(findMissingCapabilities(available, ["query", "commands", "query"])).toEqual([
      asCapabilityId("commands"),
      asCapabilityId("query"),
    ]);
  });

  it("throws a typed error containing only missing identifiers", () => {
    const available = createCapabilitySet(["events"]);

    expect(() => requireCapabilities(available, ["events", "query"])).toThrow(
      MissingCapabilitiesError,
    );
    try {
      requireCapabilities(available, ["events", "query"]);
    } catch (error) {
      expect(error).toMatchObject({ missing: [asCapabilityId("query")] });
    }
  });

  it("rejects empty and control-character capability identifiers", () => {
    expect(() => asCapabilityId("")).toThrow(InvalidCapabilityError);
    expect(() => asCapabilityId("events\nadmin")).toThrow(InvalidCapabilityError);
  });
});
