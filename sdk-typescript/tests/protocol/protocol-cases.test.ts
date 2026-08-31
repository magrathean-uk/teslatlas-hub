import { describe, expect, it, vi } from "vitest";
import eventContract from "../../protocol/source/events/teslatlas-v1.sse.json" with {
  type: "json",
};
import { TeslatlasClient } from "../../src/client/client.js";
import * as generatedProtocolCases from "../../src/generated/protocol-cases.js";
import { protocolCaseBodies, protocolCases } from "../../src/generated/protocol-cases.js";
import type { ProtocolEvent } from "../../src/protocol/models.js";
import {
  bodyForPath,
  capabilitiesForProfile,
  casesForProfile,
  loadProtocolCases,
  resolveCaseSteps,
} from "./case-loader.js";
import { runSdkProtocolCase } from "./case-runner.js";

const profiles = ["1.0.0", "1.1.0", "1.2.0"] as const;

describe("generated protocol case consumption", () => {
  it.each(profiles)(
    "loads every eligible case for profile %s as immutable resolved steps",
    (profile) => {
      const cases = casesForProfile(profile);

      expect(cases.length).toBeGreaterThan(0);
      expect(Object.isFrozen(cases)).toBe(true);
      for (const testCase of cases) {
        const steps = resolveCaseSteps(profile, testCase);
        expect(steps.length).toBeGreaterThan(0);
        expect(Object.isFrozen(steps)).toBe(true);
        expect(steps.map((step) => step.stepId)).toHaveLength(testCase.steps.length);
      }
    },
  );

  it("rejects malformed generated case data before it is used", () => {
    expect(() => loadProtocolCases([Object.freeze({ kind: "case" })])).toThrow(
      "Invalid generated protocol cases",
    );
  });

  it("rejects a case admitted before its introduced profile", () => {
    const documents = mutableDocuments();
    profileRecord(documents, "1.0.0").cases = [
      ...stringArray(profileRecord(documents, "1.0.0").cases),
      "command-idempotency",
    ];

    expect(() => loadProtocolCases(documents)).toThrow("Invalid generated protocol cases");
  });

  it("rejects a case whose required capability is absent from its profile", () => {
    const documents = mutableDocuments();
    const profile = profileRecord(documents, "1.0.0");
    profile.capabilities = stringArray(profile.capabilities).filter(
      (capability) => capability !== "query.history",
    );

    expect(() => loadProtocolCases(documents)).toThrow("Invalid generated protocol cases");
  });

  it("rejects a generated case missing its schema-required capability", () => {
    const documents = mutableDocuments();
    const command = documents.find(
      (document) => document.kind === "case" && document.case_id === "command-idempotency",
    );
    if (command === undefined) throw new Error("missing generated command case");
    delete command.capability;

    expect(() => loadProtocolCases(documents)).toThrow("Invalid generated protocol cases");
  });

  it("accepts authority-valid nested fixture paths but rejects escaping paths", () => {
    const accepted = mutableDocuments();
    replaceCommandBodyPath(accepted, "fixtures/recorded/command-job.json");
    expect(() =>
      loadProtocolCases(accepted, {
        ...protocolCaseBodies,
        "fixtures/recorded/command-job.json": protocolCaseBodies["examples/command-job.json"],
      }),
    ).not.toThrow();

    for (const path of [
      "fixtures/../command-job.json",
      "fixtures//command-job.json",
      "/fixtures/command-job.json",
      "examples\\command-job.json",
    ]) {
      const rejected = mutableDocuments();
      replaceCommandBodyPath(rejected, path);
      expect(() => loadProtocolCases(rejected)).toThrow("Invalid generated protocol cases");
    }
  });

  it("rejects duplicate generated step IDs before case resolution", () => {
    const documents = mutableDocuments();
    const steps = commandSteps(documents);
    record(steps[1]).step_id = record(steps[0]).step_id;

    expect(() => loadProtocolCases(documents)).toThrow("Invalid generated protocol cases");
  });

  it("rejects self-references instead of treating a current response as prior state", () => {
    const documents = mutableDocuments();
    const request = record(record(commandSteps(documents)[0]).request);
    request.query = { self: `\${create.status}` };

    expect(() => loadProtocolCases(documents)).toThrow("Invalid generated protocol cases");
  });

  it("resolves embedded bodies by their generated authority path", () => {
    expect(protocolCaseBodies).toHaveProperty("examples/discovery.json");
    expect(bodyForPath("examples/discovery.json")).toEqual(
      protocolCaseBodies["examples/discovery.json"],
    );
  });

  it("locks generated event names and introduced profiles to the authority event contract", () => {
    const generated = generatedProtocolCases as typeof generatedProtocolCases & {
      readonly protocolEventCatalog?: readonly unknown[];
    };
    const expected = eventContract.events.map(({ name, introduced_in }) => ({
      name,
      introduced_in,
    }));

    expect(generated.protocolEventCatalog).toEqual(expected);
    expect(Object.isFrozen(generated.protocolEventCatalog)).toBe(true);
  });

  it("exposes the declared capability set for each generated profile", () => {
    expect(capabilitiesForProfile("1.0.0")).not.toContain("commands.async");
    expect(capabilitiesForProfile("1.1.0")).toContain("commands.async");
    expect(capabilitiesForProfile("1.1.0")).not.toContain("metadata.mutable");
    expect(capabilitiesForProfile("1.2.0")).toContain("metadata.mutable");
  });

  it("models principal A and B with separate safe transcript and checkpoint traces", async () => {
    const result = await runSdkProtocolCase("1.0.0", caseFor("1.0.0", "sse-principal-visibility"));
    const evidence = result as typeof result & {
      readonly checkpoints?: readonly {
        readonly stepId: string;
        readonly principal: string;
        readonly values: readonly (string | undefined)[];
      }[];
    };
    const byStep = new Map(result.transcript.map((entry) => [entry.path, entry]));

    expect(result.failures).toEqual([]);
    expect(
      result.transcript
        .filter((entry) => entry.path === "/v1/events" || entry.path.includes("/vehicles/"))
        .map((entry) => ({
          path: entry.path,
          principal: (entry as typeof entry & { principal?: string }).principal,
        })),
    ).toEqual([
      { path: "/v1/vehicles/vehicle_demo_alpha/current", principal: "principal-a" },
      { path: "/v1/events", principal: "principal-a" },
      { path: "/v1/vehicles/vehicle_demo_alpha/current", principal: "principal-b" },
      { path: "/v1/events", principal: "principal-b" },
      { path: "/v1/events", principal: "principal-b" },
    ]);
    expect(byStep.get("/v1/events")?.authorization).toBe("present");
    expect(evidence.checkpoints).toEqual([
      { stepId: "principal-a-live", principal: "principal-a", values: ["event_demo_0042"] },
      { stepId: "principal-b-live", principal: "principal-b", values: [] },
      { stepId: "cross-principal-replay", principal: "principal-b", values: [] },
    ]);
  });

  it("fails a case whose declared response header or normalized assertion disagrees", async () => {
    const changed = structuredClone(
      caseFor("1.2.0", "metadata-if-match"),
    ) as unknown as MutableRecord;
    const read = record(commandOrMetadataSteps(changed, "read"));
    const expectRecord = record(read.expect);
    expectRecord.headers_equal = { ETag: '"wrong"' };
    expectRecord.assertions = [{ op: "equals", path: "/body/revision", value: 999 }];

    const result = await runSdkProtocolCase(
      "1.2.0",
      changed as unknown as ReturnType<typeof caseFor>,
    );

    expect(result.failures).toContain("read: response header ETag differs");
    expect(result.failures).toContain("read: assertion equals /body/revision differs");
  });

  it("records exact observed local preflight and negotiated-major error evidence", async () => {
    const discovery = await runSdkProtocolCase(
      "1.0.0",
      caseFor("1.0.0", "discovery-version-negotiation"),
    );
    const metadata = await runSdkProtocolCase("1.2.0", caseFor("1.2.0", "metadata-if-match"));
    const discoveryEvidence = discovery as typeof discovery & {
      readonly localErrors?: readonly { readonly label: string; readonly code: string }[];
    };
    const metadataEvidence = metadata as typeof metadata & {
      readonly localErrors?: readonly { readonly label: string; readonly code: string }[];
    };

    expect(discovery.failures).toEqual([]);
    expect(metadata.failures).toEqual([]);
    expect(discoveryEvidence.localErrors).toContainEqual({
      label: "unsupported-major",
      code: "incompatible_protocol",
    });
    expect(metadataEvidence.localErrors).toContainEqual({
      label: "missing-precondition",
      code: "invalid_strong_entity_tag",
    });
  });

  it("preserves the exact authority escaped deprecation regex under JavaScript Unicode regexes", async () => {
    const testCase = caseFor("1.2.0", "deprecation-sunset");
    const responseHeaders = testCase.steps.find((step) => step.stepId === "response-headers");
    if (responseHeaders === undefined) throw new Error("missing deprecation response step");
    const assertions = record(responseHeaders.expect).assertions;
    if (!Array.isArray(assertions)) throw new Error("missing deprecation assertions");
    const linkAssertion = assertions.find(
      (assertion) => record(assertion).path === "/headers/Link",
    );
    if (linkAssertion === undefined) throw new Error("missing deprecation link assertion");

    expect(record(linkAssertion).value).toBe('rel=\\"deprecation\\"');
    await expect(runSdkProtocolCase("1.2.0", testCase)).resolves.toMatchObject({ failures: [] });
  });

  it("records public retry fields from retryable problem and Retry-After evidence", async () => {
    const result = await runSdkProtocolCase("1.0.0", caseFor("1.0.0", "documented-limits"));
    const evidence = result as typeof result & {
      readonly errors?: readonly {
        readonly stepId: string;
        readonly status: number;
        readonly code: string;
        readonly retryable: boolean;
        readonly retryAfterSeconds?: number;
      }[];
    };

    expect(result.failures).toEqual([]);
    expect(evidence.errors).toContainEqual({
      stepId: "concurrency",
      status: 429,
      code: "concurrency_limit",
      retryable: true,
      retryAfterSeconds: 1,
    });
    const requestTooLarge = evidence.errors?.find((error) => error.stepId === "request-body");
    expect(requestTooLarge).toMatchObject({ retryable: false });
    expect(requestTooLarge).not.toHaveProperty("retryAfterSeconds");
  });

  it("fails when the stream emits an extra duplicate after the declared events", async () => {
    const NativeResponse = globalThis.Response;
    class ResponseWithDuplicateEvent extends NativeResponse {
      constructor(body?: BodyInit | null, init?: ResponseInit) {
        const duplicateStart =
          typeof body === "string" &&
          body.includes("id: event_demo_0042") &&
          body.includes("id: event_demo_0043")
            ? body.indexOf("id: event_demo_0043")
            : -1;
        super(
          duplicateStart < 0 || typeof body !== "string"
            ? body
            : `${body}${body.slice(duplicateStart)}`,
          init,
        );
      }
    }

    vi.stubGlobal("Response", ResponseWithDuplicateEvent);
    try {
      const result = await runSdkProtocolCase("1.0.0", caseFor("1.0.0", "sse-last-event-id"));

      expect(result.failures).toContain("initial: stream yielded an extra event");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("fails when an empty declared SSE stream yields an event", async () => {
    const NativeResponse = globalThis.Response;
    const unexpectedWire = firstProtocolEventWire();
    class ResponseWithUnexpectedEvent extends NativeResponse {
      constructor(body?: BodyInit | null, init?: ResponseInit) {
        super(body === "id:\n\n" ? `${body}${unexpectedWire}` : body, init);
      }
    }

    vi.stubGlobal("Response", ResponseWithUnexpectedEvent);
    try {
      const result = await runSdkProtocolCase("1.0.0", caseFor("1.0.0", "sse-empty-id-reset"));

      expect(result.failures).toContain("reset: stream yielded an extra event");
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("fails when a 200 SSE EOF is treated as terminal", async () => {
    const original = TeslatlasClient.prototype.streamEvents;
    const mockedStreamEvents = vi
      .spyOn(TeslatlasClient.prototype, "streamEvents")
      .mockImplementation(function (this: TeslatlasClient, options) {
        const source = original.call(this, options);
        return {
          [Symbol.asyncIterator](): AsyncIterator<ProtocolEvent> {
            const iterator = source[Symbol.asyncIterator]();
            return {
              async next(): Promise<IteratorResult<ProtocolEvent>> {
                try {
                  await iterator.next();
                } catch {
                  // Simulate a faulty client that turns the 200 EOF/reconnect into terminal completion.
                }
                return { done: true, value: undefined };
              },
            };
          },
        };
      });
    try {
      const result = await runSdkProtocolCase("1.0.0", caseFor("1.0.0", "sse-empty-id-reset"));

      expect(result.failures).toContain("reset: 200 stream ended without reconnect");
    } finally {
      mockedStreamEvents.mockRestore();
    }
  });

  for (const profile of profiles) {
    for (const testCase of casesForProfile(profile)) {
      it(`${profile} ${testCase.caseId}`, async () => {
        const result = await runSdkProtocolCase(profile, testCase);

        expect(result.failures).toEqual([]);
        expect(JSON.stringify(result.transcript)).not.toContain("Bearer ");
      });
    }
  }
});

type MutableRecord = Record<string, unknown>;

function mutableDocuments(): MutableRecord[] {
  return structuredClone(protocolCases) as MutableRecord[];
}

function profileRecord(documents: MutableRecord[], version: string): MutableRecord {
  const profile = documents.find(
    (document) => document.kind === "profile" && document.protocol_version === version,
  );
  if (profile === undefined) throw new Error("missing generated profile");
  return profile;
}

function replaceCommandBodyPath(documents: MutableRecord[], path: string): void {
  const response = record(record(commandSteps(documents)[0]).reference_response);
  response.body_file = path;
}

function commandSteps(documents: MutableRecord[]): unknown[] {
  const command = documents.find(
    (document) => document.kind === "case" && document.case_id === "command-idempotency",
  );
  const steps = command?.steps;
  if (!Array.isArray(steps) || steps.length === 0)
    throw new Error("missing generated command step");
  return steps;
}

function caseFor(profile: (typeof profiles)[number], caseId: string) {
  const testCase = casesForProfile(profile).find((candidate) => candidate.caseId === caseId);
  if (testCase === undefined) throw new Error(`missing protocol case ${caseId}`);
  return testCase;
}

function commandOrMetadataSteps(testCase: MutableRecord, stepId: string): unknown {
  const steps = testCase.steps;
  if (!Array.isArray(steps)) throw new Error("missing protocol steps");
  const step = steps.find((candidate) => record(candidate).stepId === stepId);
  if (step === undefined) throw new Error(`missing protocol step ${stepId}`);
  return step;
}

function firstProtocolEventWire(): string {
  const eventCase = caseFor("1.0.0", "sse-last-event-id");
  const initial = eventCase.steps.find((step) => step.stepId === "initial");
  if (initial === undefined) throw new Error("missing initial SSE case step");
  const events = record(initial.referenceResponse).events;
  if (!Array.isArray(events) || events.length === 0) throw new Error("missing initial SSE event");
  const event = record(events[0]);
  if (typeof event.id !== "string" || typeof event.event !== "string") {
    throw new Error("invalid initial SSE event");
  }
  return `id: ${event.id}\nevent: ${event.event}\ndata: ${JSON.stringify(event.data)}\n\n`;
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value) || value.some((entry) => typeof entry !== "string")) {
    throw new Error("expected generated string array");
  }
  return value as string[];
}

function record(value: unknown): MutableRecord {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("expected generated record");
  }
  return value as MutableRecord;
}
