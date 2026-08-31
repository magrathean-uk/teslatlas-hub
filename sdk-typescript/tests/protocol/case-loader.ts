import { protocolCaseBodies, protocolCases } from "../../src/generated/protocol-cases.js";

export type ProtocolProfile = "1.0.0" | "1.1.0" | "1.2.0";

type JsonValue = null | boolean | number | string | JsonArray | JsonRecord;

interface JsonArray extends ReadonlyArray<JsonValue> {}

interface JsonRecord {
  readonly [key: string]: JsonValue;
}

export interface ProtocolCaseStep {
  readonly stepId: string;
  readonly request: JsonRecord;
  readonly expect: JsonRecord;
  readonly referenceResponse: JsonRecord;
}

export interface ProtocolCase {
  readonly caseId: string;
  readonly introducedIn: ProtocolProfile;
  readonly capability: string;
  readonly steps: readonly ProtocolCaseStep[];
}

export interface ResolvedCaseStep {
  readonly stepId: string;
  readonly request: JsonRecord;
  readonly expect: JsonRecord;
  readonly response: JsonRecord;
}

export interface ProtocolCaseRegistry {
  readonly casesById: ReadonlyMap<string, ProtocolCase>;
  readonly profileCaseIds: ReadonlyMap<ProtocolProfile, readonly string[]>;
  readonly profileCapabilities: ReadonlyMap<ProtocolProfile, readonly string[]>;
  readonly bodies: ReadonlyMap<string, JsonValue>;
}

const profiles = ["1.0.0", "1.1.0", "1.2.0"] as const;
const profileSet = new Set<string>(profiles);
const templatePattern = /^\$\{([^}]+)\}$/u;
const bodyPathPattern =
  /^(?:examples|fixtures)\/(?:[A-Za-z0-9][A-Za-z0-9._-]*\/)*[A-Za-z0-9][A-Za-z0-9._-]*$/u;

const registry = loadProtocolCases();

export function casesForProfile(profile: ProtocolProfile): readonly ProtocolCase[] {
  const caseIds = registry.profileCaseIds.get(profile);
  if (caseIds === undefined) {
    throw invalid();
  }
  return Object.freeze(
    caseIds.map((caseId) => {
      const value = registry.casesById.get(caseId);
      if (value === undefined) throw invalid();
      return value;
    }),
  );
}

export function capabilitiesForProfile(profile: ProtocolProfile): readonly string[] {
  const capabilities = registry.profileCapabilities.get(profile);
  if (capabilities === undefined) throw invalid();
  return Object.freeze([...capabilities]);
}

export function bodyForPath(path: string): unknown {
  assertBodyPath(path);
  const body = registry.bodies.get(path);
  if (body === undefined) throw invalid();
  return cloneJson(body);
}

export function resolveCaseSteps(
  profile: ProtocolProfile,
  testCase: ProtocolCase,
): readonly ResolvedCaseStep[] {
  assertProfile(profile);
  const context: Record<string, JsonRecord> = Object.create(null) as Record<string, JsonRecord>;
  const resolved: ResolvedCaseStep[] = [];
  for (const step of testCase.steps) {
    const response = materializeBody(
      expectRecord(resolveTemplates(step.referenceResponse, profile, context)),
    );
    const request = expectRecord(resolveTemplates(step.request, profile, context));
    const expected = expectRecord(resolveTemplates(step.expect, profile, context));
    context[step.stepId] = response;
    resolved.push(
      Object.freeze({
        stepId: step.stepId,
        request,
        expect: expected,
        response,
      }),
    );
  }
  return Object.freeze(resolved);
}

export function loadProtocolCases(
  documents: readonly unknown[] = protocolCases,
  embeddedBodies: unknown = protocolCaseBodies,
): ProtocolCaseRegistry {
  if (!Array.isArray(documents)) throw invalid();
  const copied = documents.map((document) => cloneJson(document));
  const manifest = onlyRecord(copied, "manifest");
  validateManifest(manifest);

  const profilesByVersion = new Map<ProtocolProfile, JsonRecord>();
  const caseRecords = new Map<string, JsonRecord>();
  for (const document of copied) {
    const record = asRecord(document);
    if (record === undefined) throw invalid();
    const kind = record.kind;
    if (kind === "manifest") continue;
    if (kind === "profile") {
      const version = profileValue(record.protocol_version);
      if (profilesByVersion.has(version)) throw invalid();
      profilesByVersion.set(version, record);
      continue;
    }
    if (kind === "case") {
      const caseId = stringValue(record.case_id);
      if (caseRecords.has(caseId)) throw invalid();
      caseRecords.set(caseId, record);
      continue;
    }
    throw invalid();
  }
  if (profilesByVersion.size !== profiles.length || caseRecords.size === 0) throw invalid();

  const bodyPaths = [...collectBodyPaths(caseRecords.values())].sort();
  const bodies = parseBodies(embeddedBodies, bodyPaths);

  const casesById = new Map<string, ProtocolCase>();
  for (const [caseId, record] of caseRecords) {
    const steps = arrayValue(record.steps).map((step) => parseStep(step));
    if (steps.length === 0) throw invalid();
    validateStepTopology(steps);
    const introducedIn = profileValue(record.introduced_in);
    const capability = stringValue(record.capability);
    casesById.set(
      caseId,
      Object.freeze({
        caseId,
        introducedIn,
        capability,
        steps: Object.freeze(steps),
      }),
    );
  }

  const profileCaseIds = new Map<ProtocolProfile, readonly string[]>();
  const profileCapabilities = new Map<ProtocolProfile, readonly string[]>();
  for (const profile of profiles) {
    const record = profilesByVersion.get(profile);
    if (record === undefined) throw invalid();
    const caseIds = arrayValue(record.cases).map((value) => stringValue(value));
    const capabilities = arrayValue(record.capabilities).map((value) => stringValue(value));
    if (
      new Set(caseIds).size !== caseIds.length ||
      new Set(capabilities).size !== capabilities.length ||
      caseIds.some((caseId) => {
        const testCase = casesById.get(caseId);
        return (
          testCase === undefined ||
          profileIndex(testCase.introducedIn) > profileIndex(profile) ||
          !capabilities.includes(testCase.capability)
        );
      })
    ) {
      throw invalid();
    }
    profileCaseIds.set(profile, Object.freeze([...caseIds]));
    profileCapabilities.set(profile, Object.freeze([...capabilities]));
  }

  return Object.freeze({
    casesById: freezeMap(casesById),
    profileCaseIds: freezeMap(profileCaseIds),
    profileCapabilities: freezeMap(profileCapabilities),
    bodies: freezeMap(bodies),
  });
}

function parseStep(value: JsonValue): ProtocolCaseStep {
  const record = expectRecord(value);
  return Object.freeze({
    stepId: stringValue(record.step_id),
    request: expectRecord(record.request),
    expect: expectRecord(record.expect),
    referenceResponse: expectRecord(record.reference_response),
  });
}

function validateStepTopology(steps: readonly ProtocolCaseStep[]): void {
  const priorStepIds = new Set<string>();
  for (const step of steps) {
    if (priorStepIds.has(step.stepId)) throw invalid();
    validateTemplateReferences(step.referenceResponse, priorStepIds);
    validateTemplateReferences(step.request, priorStepIds);
    validateTemplateReferences(step.expect, priorStepIds);
    priorStepIds.add(step.stepId);
  }
}

function validateTemplateReferences(value: JsonValue, priorStepIds: ReadonlySet<string>): void {
  if (typeof value === "string") {
    const match = templatePattern.exec(value);
    if (match === null) {
      if (value.includes("${")) throw invalid();
      return;
    }
    const reference = match[1];
    if (reference === undefined || reference === "profile") return;
    const [stepId] = reference.split(".");
    if (stepId === undefined || stepId.length === 0 || !priorStepIds.has(stepId)) throw invalid();
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) validateTemplateReferences(item, priorStepIds);
    return;
  }
  const record = asRecord(value);
  if (record === undefined) return;
  for (const item of Object.values(record)) validateTemplateReferences(item, priorStepIds);
}

function validateManifest(manifest: JsonRecord): void {
  const supported = arrayValue(manifest.supported_profiles).map((value) => profileValue(value));
  if (
    supported.length !== profiles.length ||
    supported.some((profile, index) => profile !== profiles[index])
  ) {
    throw invalid();
  }
  const manifestProfiles = arrayValue(manifest.profiles);
  if (manifestProfiles.length !== profiles.length) throw invalid();
  for (const entry of manifestProfiles) {
    const record = expectRecord(entry);
    const version = profileValue(record.version);
    const path = stringValue(record.path);
    if (path !== `compatibility/${version}/profile.json`) throw invalid();
  }
}

function materializeBody(response: JsonRecord): JsonRecord {
  if (!Object.hasOwn(response, "body_file")) return response;
  if (Object.hasOwn(response, "body")) throw invalid();
  const bodyFile = stringValue(response.body_file);
  assertBodyPath(bodyFile);
  const body = registry.bodies.get(bodyFile);
  if (body === undefined) throw invalid();
  const materialized: Record<string, JsonValue> = Object.create(null) as Record<string, JsonValue>;
  for (const [key, value] of Object.entries(response)) {
    if (key !== "body_file") materialized[key] = value;
  }
  materialized.body = cloneJson(body);
  return freezeRecord(materialized);
}

function resolveTemplates(
  value: JsonValue,
  profile: ProtocolProfile,
  context: Readonly<Record<string, JsonRecord>>,
): JsonValue {
  if (typeof value === "string") {
    const match = templatePattern.exec(value);
    if (match === null) {
      if (value.includes("${")) throw invalid();
      return value;
    }
    const reference = match[1];
    if (reference === undefined) throw invalid();
    if (reference === "profile") return profile;
    return cloneJson(lookupReference(context, reference));
  }
  if (Array.isArray(value)) {
    return freezeArray(value.map((item) => resolveTemplates(item, profile, context)));
  }
  if (value !== null && typeof value === "object") {
    const output: Record<string, JsonValue> = Object.create(null) as Record<string, JsonValue>;
    for (const [key, item] of Object.entries(value)) {
      output[key] = resolveTemplates(item, profile, context);
    }
    return freezeRecord(output);
  }
  return value;
}

function lookupReference(
  context: Readonly<Record<string, JsonRecord>>,
  reference: string,
): JsonValue {
  const [first, ...rest] = reference.split(".");
  if (first === undefined || first.length === 0) throw invalid();
  const initial = context[first];
  if (initial === undefined) throw invalid();
  let current: JsonValue = initial;
  for (const part of rest) {
    if (Array.isArray(current)) {
      if (!/^(0|[1-9][0-9]*)$/u.test(part)) throw invalid();
      const item: JsonValue | undefined = (current as JsonArray)[Number(part)];
      if (item === undefined) throw invalid();
      current = item;
      continue;
    }
    const record = asRecord(current);
    if (record === undefined || !Object.hasOwn(record, part)) throw invalid();
    current = record[part] as JsonValue;
  }
  return current;
}

function collectBodyPaths(records: Iterable<JsonRecord>): Set<string> {
  const paths = new Set<string>();
  for (const record of records) collectBodyPathsFromValue(record, paths);
  return paths;
}

function parseBodies(value: unknown, paths: readonly string[]): Map<string, JsonValue> {
  const copied = expectRecord(cloneJson(value));
  const keys = Object.keys(copied).sort();
  if (keys.length !== paths.length || keys.some((key, index) => key !== paths[index])) {
    throw invalid();
  }
  const bodies = new Map<string, JsonValue>();
  for (const path of keys) {
    assertBodyPath(path);
    const body = copied[path];
    if (body === undefined) throw invalid();
    bodies.set(path, body);
  }
  return bodies;
}

function collectBodyPathsFromValue(value: JsonValue, paths: Set<string>): void {
  if (Array.isArray(value)) {
    for (const item of value) collectBodyPathsFromValue(item, paths);
    return;
  }
  const record = asRecord(value);
  if (record === undefined) return;
  if (Object.hasOwn(record, "body_file")) {
    const path = stringValue(record.body_file);
    assertBodyPath(path);
    paths.add(path);
  }
  for (const item of Object.values(record)) collectBodyPathsFromValue(item, paths);
}

function onlyRecord(values: readonly JsonValue[], kind: string): JsonRecord {
  const matches = values.filter((value) => asRecord(value)?.kind === kind);
  if (matches.length !== 1) throw invalid();
  return expectRecord(matches[0]);
}

function cloneJson(value: unknown, ancestors = new Set<object>()): JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw invalid();
    return value;
  }
  if (typeof value !== "object" || ancestors.has(value)) throw invalid();
  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      if (Object.getPrototypeOf(value) !== Array.prototype) throw invalid();
      return freezeArray(value.map((item) => cloneJson(item, ancestors)));
    }
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) throw invalid();
    const output: Record<string, JsonValue> = Object.create(null) as Record<string, JsonValue>;
    for (const key of Reflect.ownKeys(value)) {
      if (typeof key !== "string") throw invalid();
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (descriptor?.enumerable !== true || !("value" in descriptor)) throw invalid();
      output[key] = cloneJson(descriptor.value, ancestors);
    }
    return freezeRecord(output);
  } finally {
    ancestors.delete(value);
  }
}

function asRecord(value: JsonValue | unknown): JsonRecord | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonRecord)
    : undefined;
}

function expectRecord(value: JsonValue | unknown): JsonRecord {
  const record = asRecord(value);
  if (record === undefined) throw invalid();
  return record;
}

function arrayValue(value: JsonValue | unknown): JsonArray {
  if (!Array.isArray(value)) throw invalid();
  return value;
}

function stringValue(value: JsonValue | unknown): string {
  if (typeof value !== "string" || value.length === 0) throw invalid();
  return value;
}

function profileValue(value: JsonValue | unknown): ProtocolProfile {
  const profile = stringValue(value);
  if (!profileSet.has(profile)) throw invalid();
  return profile as ProtocolProfile;
}

function assertProfile(value: string): asserts value is ProtocolProfile {
  if (!profileSet.has(value)) throw invalid();
}

function assertBodyPath(value: string): void {
  if (!bodyPathPattern.test(value) || value.startsWith("/") || value.includes("\\")) {
    throw invalid();
  }
}

function profileIndex(profile: ProtocolProfile): number {
  const index = profiles.indexOf(profile);
  if (index === -1) throw invalid();
  return index;
}

function freezeArray(values: JsonValue[]): JsonArray {
  return Object.freeze(values);
}

function freezeRecord(values: Record<string, JsonValue>): JsonRecord {
  return Object.freeze(values);
}

function freezeMap<Key, Value>(values: Map<Key, Value>): ReadonlyMap<Key, Value> {
  const snapshot = new Map(values);
  return Object.freeze({
    get: snapshot.get.bind(snapshot),
    has: snapshot.has.bind(snapshot),
    entries: snapshot.entries.bind(snapshot),
    keys: snapshot.keys.bind(snapshot),
    values: snapshot.values.bind(snapshot),
    forEach: snapshot.forEach.bind(snapshot),
    get size() {
      return snapshot.size;
    },
    [Symbol.iterator]: snapshot[Symbol.iterator].bind(snapshot),
  }) as ReadonlyMap<Key, Value>;
}

function invalid(): Error {
  return new Error("Invalid generated protocol cases");
}
