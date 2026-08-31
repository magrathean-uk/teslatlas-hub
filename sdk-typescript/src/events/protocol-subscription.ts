import {
  ProtocolHttpError,
  ProtocolValidationError,
  ReplayGapError,
  TeslatlasError,
  TransportError,
  containsControlCharacters,
} from "../core/errors.js";
import { protocolEventCatalog } from "../generated/protocol-cases.js";
import { validateEvent } from "../generated/validators.js";
import { decodeProtocolProblemResponse } from "../http/response-decoder.js";
import type { ClientSession } from "../client/types.js";
import { requireCapability } from "../protocol/capabilities.js";
import type { ProtocolEvent } from "../protocol/models.js";
import type { SupportedProtocolVersion } from "../protocol/negotiation.js";
import { decodeProtocolValue } from "../protocol/validate.js";
import {
  subscribeToSse,
  type SseCheckpointStore,
  type SseEvent,
  type SseResponseAction,
  type SseSleep,
} from "./sse-subscription.js";

const defaultReconnectMilliseconds = 3_000;
const maximumReconnectMilliseconds = 30_000;

const eventIntroducedIn = generatedEventCatalog(protocolEventCatalog);

const resourceIdentifierField = Object.freeze({
  "observation.admitted": "observation_id",
  "vehicle.current.changed": "vehicle_id",
  "drive.started": "drive_id",
  "drive.updated": "drive_id",
  "drive.ended": "drive_id",
  "charge.started": "charge_id",
  "charge.updated": "charge_id",
  "charge.ended": "charge_id",
  "state.changed": "state_id",
  "software_update.changed": "update_id",
  "data_quality.changed": "subject_id",
  "command.changed": "command_id",
  "metadata.changed": "metadata_id",
} as const satisfies Readonly<Record<ProtocolEvent["event_type"], string>>);

export interface StreamEventsOptions {
  readonly vehicleId?: string;
  readonly eventTypes?: readonly ProtocolEvent["event_type"][];
  readonly checkpoint?: SseCheckpointStore;
  readonly signal?: AbortSignal;
  readonly sleep?: SseSleep;
}

export class InvalidStreamEventsOptionsError extends TeslatlasError<"invalid_stream_events_options"> {
  constructor() {
    super("Teslatlas event stream options are invalid", { code: "invalid_stream_events_options" });
  }
}

export class UnsupportedStreamEventTypeError extends TeslatlasError<"unsupported_stream_event_type"> {
  constructor() {
    super("Teslatlas event type is unavailable in the negotiated protocol profile", {
      code: "unsupported_stream_event_type",
    });
  }
}

export function streamProtocolEvents(
  session: ClientSession,
  options: StreamEventsOptions = {},
): AsyncIterable<ProtocolEvent> {
  requireCapability(session.descriptor, "events.sse");
  const normalized = normalizeOptions(options, session.protocolVersion);
  return subscribeToSse<ProtocolEvent>({
    transport: session.eventTransport,
    path: eventPath(normalized.vehicleId, normalized.eventTypes),
    redirect: "error",
    protocolVersion: session.protocolVersion,
    ...(normalized.checkpoint === undefined ? {} : { checkpoint: normalized.checkpoint }),
    ...(normalized.signal === undefined ? {} : { signal: normalized.signal }),
    ...(normalized.sleep === undefined ? {} : { sleep: normalized.sleep }),
    maximumServerRetryMilliseconds: maximumReconnectMilliseconds,
    responseClassifier: (response) => classifyProtocolResponse(response, normalized.signal),
    eventMapper: (event) => decodeProtocolEvent(event, session.protocolVersion),
    reconnect: ({ reason, error }) => {
      if (reason === "eof" || error instanceof TransportError) {
        return defaultReconnectMilliseconds;
      }
      return undefined;
    },
  });
}

interface NormalizedStreamEventsOptions {
  readonly vehicleId?: string;
  readonly eventTypes: readonly ProtocolEvent["event_type"][];
  readonly checkpoint?: SseCheckpointStore;
  readonly signal?: AbortSignal;
  readonly sleep?: SseSleep;
}

function normalizeOptions(
  options: StreamEventsOptions,
  protocolVersion: SupportedProtocolVersion,
): NormalizedStreamEventsOptions {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new InvalidStreamEventsOptionsError();
  }
  const vehicleId = normalizeVehicleId(options.vehicleId);
  const eventTypes = normalizeEventTypes(options.eventTypes, protocolVersion);
  if (
    options.checkpoint !== undefined &&
    (options.checkpoint === null || typeof options.checkpoint !== "object")
  ) {
    throw new InvalidStreamEventsOptionsError();
  }
  if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) {
    throw new InvalidStreamEventsOptionsError();
  }
  if (options.sleep !== undefined && typeof options.sleep !== "function") {
    throw new InvalidStreamEventsOptionsError();
  }
  return {
    ...(vehicleId === undefined ? {} : { vehicleId }),
    eventTypes,
    ...(options.checkpoint === undefined ? {} : { checkpoint: options.checkpoint }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
    ...(options.sleep === undefined ? {} : { sleep: options.sleep }),
  };
}

function normalizeVehicleId(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  if (
    typeof value !== "string" ||
    value.length < 3 ||
    value.length > 128 ||
    containsControlCharacters(value)
  ) {
    throw new InvalidStreamEventsOptionsError();
  }
  return value;
}

function normalizeEventTypes(
  value: readonly ProtocolEvent["event_type"][] | undefined,
  protocolVersion: SupportedProtocolVersion,
): readonly ProtocolEvent["event_type"][] {
  if (value === undefined) return Object.freeze([]);
  if (!Array.isArray(value) || value.length > 32) {
    throw new InvalidStreamEventsOptionsError();
  }
  const seen = new Set<ProtocolEvent["event_type"]>();
  const normalized: ProtocolEvent["event_type"][] = [];
  for (const eventType of value) {
    if (typeof eventType !== "string") {
      throw new InvalidStreamEventsOptionsError();
    }
    const typedEventType = eventType as ProtocolEvent["event_type"];
    const introducedIn = eventIntroducedIn.get(typedEventType);
    if (introducedIn === undefined) throw new InvalidStreamEventsOptionsError();
    if (seen.has(typedEventType)) {
      throw new InvalidStreamEventsOptionsError();
    }
    if (compareProfiles(introducedIn, protocolVersion) > 0) {
      throw new UnsupportedStreamEventTypeError();
    }
    seen.add(typedEventType);
    normalized.push(typedEventType);
  }
  return Object.freeze(normalized);
}

function eventPath(
  vehicleId: string | undefined,
  eventTypes: readonly ProtocolEvent["event_type"][],
): string {
  const query = new URLSearchParams();
  if (vehicleId !== undefined) query.append("vehicle_id", vehicleId);
  if (eventTypes.length > 0) query.append("event_type", eventTypes.join(","));
  const encoded = query.toString();
  return encoded.length === 0 ? "/v1/events" : `/v1/events?${encoded}`;
}

async function classifyProtocolResponse(
  response: Response,
  signal: AbortSignal | undefined,
): Promise<SseResponseAction> {
  if (response.status === 200) return "continue";
  if (response.status === 204) {
    if (response.body !== null) {
      return { error: new ProtocolValidationError("streamEvents.204") };
    }
    return "terminal";
  }
  try {
    await decodeProtocolProblemResponse(response, signal);
  } catch (error) {
    if (
      error instanceof ProtocolHttpError &&
      error.status === 410 &&
      error.code === "event_replay_expired"
    ) {
      return { error: new ReplayGapError(error.status, error.requestId) };
    }
    return { error };
  }
  return { error: new ProtocolValidationError("streamEvents.response") };
}

function decodeProtocolEvent(
  event: SseEvent,
  protocolVersion: SupportedProtocolVersion,
): ProtocolEvent | undefined {
  const eventType = event.event as ProtocolEvent["event_type"];
  const introducedIn = eventIntroducedIn.get(eventType);
  if (introducedIn === undefined) return undefined;
  if (compareProfiles(introducedIn, protocolVersion) > 0) {
    throw new ProtocolValidationError("validateEvent.profile");
  }

  let rawValue: unknown;
  try {
    rawValue = JSON.parse(event.data) as unknown;
  } catch {
    throw new ProtocolValidationError("validateEvent.json");
  }
  const value = decodeProtocolValue<ProtocolEvent>(rawValue, validateEvent, "validateEvent");
  if (value.event_id !== event.lastEventId) {
    throw new ProtocolValidationError("validateEvent.id");
  }
  if (value.event_type !== event.event) {
    throw new ProtocolValidationError("validateEvent.type");
  }
  assertEventSemantics(value);
  return value;
}

function assertEventSemantics(event: ProtocolEvent): void {
  const data = asRecord(event.data);
  if (data === undefined) {
    throw new ProtocolValidationError("validateEvent.data");
  }

  const revision = eventRevision(data);
  if (revision !== undefined && event.revision !== revision) {
    throw new ProtocolValidationError("validateEvent.revision");
  }
  if (Object.hasOwn(data, "vehicle_id") && event.vehicle_id !== data.vehicle_id) {
    throw new ProtocolValidationError("validateEvent.vehicle_id");
  }
  const resourceField = resourceIdentifierField[event.event_type];
  if (Object.hasOwn(data, resourceField) && event.resource_id !== data[resourceField]) {
    throw new ProtocolValidationError("validateEvent.resource_id");
  }
}

function eventRevision(data: Record<string, unknown>): unknown {
  if (data.revision !== undefined) return data.revision;
  const audit = asRecord(data.audit);
  const deletion = audit === undefined ? undefined : asRecord(audit.deletion);
  return deletion?.revision;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function generatedEventCatalog(
  value: unknown,
): ReadonlyMap<ProtocolEvent["event_type"], SupportedProtocolVersion> {
  if (!Array.isArray(value)) throw new Error("Generated event catalog is invalid");
  const catalog = new Map<ProtocolEvent["event_type"], SupportedProtocolVersion>();
  for (const entry of value) {
    const record = asRecord(entry);
    const name = record?.name;
    const introducedIn = record?.introduced_in;
    if (
      typeof name !== "string" ||
      typeof introducedIn !== "string" ||
      !isSupportedProfile(introducedIn) ||
      catalog.has(name as ProtocolEvent["event_type"])
    ) {
      throw new Error("Generated event catalog is invalid");
    }
    catalog.set(name as ProtocolEvent["event_type"], introducedIn);
  }
  return catalog;
}

function isSupportedProfile(value: string): value is SupportedProtocolVersion {
  return value === "1.0.0" || value === "1.1.0" || value === "1.2.0";
}

function compareProfiles(left: SupportedProtocolVersion, right: SupportedProtocolVersion): number {
  const [leftMajor, leftMinor, leftPatch] = left.split(".").map(Number);
  const [rightMajor, rightMinor, rightPatch] = right.split(".").map(Number);
  return (
    (leftMajor ?? 0) - (rightMajor ?? 0) ||
    (leftMinor ?? 0) - (rightMinor ?? 0) ||
    (leftPatch ?? 0) - (rightPatch ?? 0)
  );
}
