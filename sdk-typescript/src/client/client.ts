import {
  validateCommandJob,
  validateCommandRequest,
  validateCharge,
  validateChargePage,
  validateChargeSamplePage,
  validateCurrentState,
  validateDataQualityPage,
  validateDiscovery,
  validateDrive,
  validateDrivePage,
  validateMetadataCreate,
  validateMetadataPage,
  validateMetadataRecord,
  validateMetadataReplace,
  validateMetadataTombstone,
  validatePositionPage,
  validateStatePage,
  validateUpdatePage,
  validateVehiclePage,
} from "../generated/validators.js";
import { validateAdvertisedCommand } from "../commands/advertised-command.js";
import {
  asEntityTag,
  asOpaqueCursor,
  type EntityTag,
  type OpaqueCursor,
} from "../core/opaque-values.js";
import {
  CommandUncertainError,
  containsControlCharacters,
  ProtocolHttpError,
  ProtocolValidationError,
} from "../core/errors.js";
import {
  buildWriteRequest,
  buildReadRequest,
  readOperationDescriptors,
  type ReadOperationName,
  writeOperationDescriptors,
  type WriteOperationName,
  type WriteRequestOptions,
} from "../http/request-builder.js";
import {
  decodeReadResponse,
  decodeWriteResponse,
  type ReadResponseRequirements,
  type WriteResponseRequirements,
} from "../http/response-decoder.js";
import { asIdempotencyKey } from "../commands/idempotency.js";
import { asStrongEntityTag } from "../http/strong-etag.js";
import {
  InvalidRequestBodyError,
  snapshotJsonRequest,
  type JsonRequestSnapshot,
} from "../http/json-request-snapshot.js";
import { decodeProtocolValue, type ProtocolValidator } from "../protocol/validate.js";
import { requireCapability } from "../protocol/capabilities.js";
import type {
  Charge,
  ChargePage,
  ChargeSamplePage,
  CommandJob,
  CommandRequest,
  CurrentState,
  DataQualityPage,
  Drive,
  DrivePage,
  HubDescriptor,
  MetadataCreate,
  MetadataPage,
  MetadataRecord,
  MetadataReplace,
  MetadataTombstone,
  PositionPage,
  StatePage,
  UpdatePage,
  VehiclePage,
} from "../protocol/models.js";
import type { ClientSession } from "./types.js";
import {
  negotiateProtocolVersion,
  type SupportedProtocolVersion,
} from "../protocol/negotiation.js";
import {
  InvalidReadOptionsError,
  type ConditionalReadOptions,
  type CommandCreateOptions,
  type DataQualityPageOptions,
  type HistoryPageOptions,
  type IfMatchOptions,
  type MetadataPageOptions,
  type PageReadOptions,
  type ReadResult,
  type RequestOptions,
  type WriteResult,
} from "./operations.js";
import { streamProtocolEvents, type StreamEventsOptions } from "../events/protocol-subscription.js";

type QueryValue = string | number | OpaqueCursor | undefined;
type QueryValues = Readonly<Record<string, QueryValue>>;
type RangeLimit = "history" | "dense";
const maximumIfNoneMatchLength = 512;

export class TeslatlasClient {
  readonly #session: ClientSession;

  constructor(session: ClientSession) {
    this.#session = session;
  }

  get descriptor(): HubDescriptor {
    return this.#session.descriptor;
  }

  get protocolVersion(): SupportedProtocolVersion {
    return this.#session.protocolVersion;
  }

  streamEvents(
    options: StreamEventsOptions = {},
  ): AsyncIterable<import("../protocol/models.js").ProtocolEvent> {
    return streamProtocolEvents(this.#session, options);
  }

  async discoverHub(options: ConditionalReadOptions = {}): Promise<ReadResult<HubDescriptor>> {
    return this.#read(
      "discoverHub",
      validateDiscovery,
      "validateDiscovery",
      {},
      {},
      normalizeConditionalOptions(options),
    );
  }

  async listVehicles(options: PageReadOptions = {}): Promise<ReadResult<VehiclePage>> {
    requireCapability(this.#session.descriptor, "query.vehicles");
    const normalized = this.#normalizePageOptions(options);
    return this.#read(
      "listVehicles",
      validateVehiclePage,
      "validateVehiclePage",
      {},
      pageQuery(normalized),
      normalized,
    );
  }

  async getVehicleCurrentState(
    vehicleId: string,
    options: ConditionalReadOptions = {},
  ): Promise<ReadResult<CurrentState>> {
    requireCapability(this.#session.descriptor, "query.vehicles");
    return this.#read(
      "getVehicleCurrentState",
      validateCurrentState,
      "validateCurrentState",
      { vehicle_id: validateId(vehicleId) },
      {},
      normalizeConditionalOptions(options),
    );
  }

  async listVehicleDrives(
    vehicleId: string,
    options: HistoryPageOptions = {},
  ): Promise<ReadResult<DrivePage>> {
    return this.#historyRead(
      "listVehicleDrives",
      validateDrivePage,
      "validateDrivePage",
      { vehicle_id: validateId(vehicleId) },
      options,
      "history",
    );
  }

  async getDrive(
    driveId: string,
    options: ConditionalReadOptions = {},
  ): Promise<ReadResult<Drive>> {
    requireCapability(this.#session.descriptor, "query.history");
    return this.#read(
      "getDrive",
      validateDrive,
      "validateDrive",
      { drive_id: validateId(driveId) },
      {},
      normalizeConditionalOptions(options),
    );
  }

  async listDrivePositions(
    driveId: string,
    options: HistoryPageOptions = {},
  ): Promise<ReadResult<PositionPage>> {
    return this.#historyRead(
      "listDrivePositions",
      validatePositionPage,
      "validatePositionPage",
      { drive_id: validateId(driveId) },
      options,
      "dense",
    );
  }

  async listVehicleCharges(
    vehicleId: string,
    options: HistoryPageOptions = {},
  ): Promise<ReadResult<ChargePage>> {
    return this.#historyRead(
      "listVehicleCharges",
      validateChargePage,
      "validateChargePage",
      { vehicle_id: validateId(vehicleId) },
      options,
      "history",
    );
  }

  async getCharge(
    chargeId: string,
    options: ConditionalReadOptions = {},
  ): Promise<ReadResult<Charge>> {
    requireCapability(this.#session.descriptor, "query.history");
    return this.#read(
      "getCharge",
      validateCharge,
      "validateCharge",
      { charge_id: validateId(chargeId) },
      {},
      normalizeConditionalOptions(options),
    );
  }

  async listChargeSamples(
    chargeId: string,
    options: HistoryPageOptions = {},
  ): Promise<ReadResult<ChargeSamplePage>> {
    return this.#historyRead(
      "listChargeSamples",
      validateChargeSamplePage,
      "validateChargeSamplePage",
      { charge_id: validateId(chargeId) },
      options,
      "dense",
    );
  }

  async listVehicleStates(
    vehicleId: string,
    options: HistoryPageOptions = {},
  ): Promise<ReadResult<StatePage>> {
    return this.#historyRead(
      "listVehicleStates",
      validateStatePage,
      "validateStatePage",
      { vehicle_id: validateId(vehicleId) },
      options,
      "history",
    );
  }

  async listVehicleUpdates(
    vehicleId: string,
    options: HistoryPageOptions = {},
  ): Promise<ReadResult<UpdatePage>> {
    return this.#historyRead(
      "listVehicleUpdates",
      validateUpdatePage,
      "validateUpdatePage",
      { vehicle_id: validateId(vehicleId) },
      options,
      "history",
    );
  }

  async listDataQuality(
    options: DataQualityPageOptions = {},
  ): Promise<ReadResult<DataQualityPage>> {
    requireCapability(this.#session.descriptor, "data-quality");
    const normalized = this.#normalizeHistoryOptions(options, "history");
    const vehicleId = options.vehicleId === undefined ? undefined : validateId(options.vehicleId);
    return this.#read(
      "listDataQuality",
      validateDataQualityPage,
      "validateDataQualityPage",
      {},
      { ...historyQuery(normalized), vehicle_id: vehicleId },
      normalized,
    );
  }

  async listVehicleMetadata(
    vehicleId: string,
    options: MetadataPageOptions = {},
  ): Promise<ReadResult<MetadataPage>> {
    requireCapability(this.#session.descriptor, "metadata.mutable");
    const normalized = this.#normalizeMetadataPageOptions(options);
    return this.#read(
      "listVehicleMetadata",
      validateMetadataPage,
      "validateMetadataPage",
      { vehicle_id: validateId(vehicleId) },
      {
        cursor: normalized.cursor,
        limit: normalized.limit,
        kind: normalized.kind,
      },
      normalized,
    );
  }

  async createMetadata(
    vehicleId: string,
    body: MetadataCreate,
    options: RequestOptions = {},
  ): Promise<WriteResult<MetadataRecord>> {
    requireCapability(this.#session.descriptor, "metadata.mutable");
    const normalizedOptions = normalizeRequestOptions(options);
    throwIfAlreadyAborted(normalizedOptions.signal);
    const validatedVehicleId = validateId(vehicleId);
    const snapshot = snapshotJsonRequest(
      body,
      this.#session.descriptor.limits.max_request_body_bytes,
    );
    const value = decodeProtocolValue<MetadataCreate>(
      snapshot.value,
      validateMetadataCreate,
      "validateMetadataCreate",
    );
    if (value.vehicle_id !== validatedVehicleId) {
      throw new ProtocolValidationError("validateMetadataCreate.vehicle_id");
    }
    return this.#write(
      "createMetadata",
      validateMetadataRecord,
      "validateMetadataRecord",
      { vehicle_id: validatedVehicleId },
      {
        body: snapshot,
        ...(normalizedOptions.signal === undefined ? {} : { signal: normalizedOptions.signal }),
      },
      { successStatus: 201, requireStrongEntityTag: true, requireLocation: true },
    );
  }

  async getMetadata(
    metadataId: string,
    options: ConditionalReadOptions = {},
  ): Promise<ReadResult<MetadataRecord | MetadataTombstone>> {
    requireCapability(this.#session.descriptor, "metadata.mutable");
    return this.#read(
      "getMetadata",
      validateMetadataEntity,
      "validateMetadataEntity",
      { metadata_id: validateId(metadataId) },
      {},
      normalizeConditionalOptions(options),
      { requireStrongEntityTag: true },
    );
  }

  async replaceMetadata(
    metadataId: string,
    body: MetadataReplace,
    options: IfMatchOptions,
  ): Promise<WriteResult<MetadataRecord>> {
    requireCapability(this.#session.descriptor, "metadata.mutable");
    const ifMatch = asStrongEntityTag(options?.ifMatch as string);
    const signal = options?.signal;
    throwIfAlreadyAborted(signal);
    const snapshot = snapshotJsonRequest(
      body,
      this.#session.descriptor.limits.max_request_body_bytes,
    );
    decodeProtocolValue<MetadataReplace>(
      snapshot.value,
      validateMetadataReplace,
      "validateMetadataReplace",
    );
    return this.#write(
      "replaceMetadata",
      validateMetadataRecord,
      "validateMetadataRecord",
      { metadata_id: validateId(metadataId) },
      {
        body: snapshot,
        ifMatch,
        ...(signal === undefined ? {} : { signal }),
      },
      { successStatus: 200, requireStrongEntityTag: true },
    );
  }

  async deleteMetadata(
    metadataId: string,
    options: IfMatchOptions,
  ): Promise<WriteResult<MetadataTombstone>> {
    requireCapability(this.#session.descriptor, "metadata.mutable");
    const ifMatch = asStrongEntityTag(options?.ifMatch as string);
    const signal = options?.signal;
    throwIfAlreadyAborted(signal);
    return this.#write(
      "deleteMetadata",
      validateMetadataTombstone,
      "validateMetadataTombstone",
      { metadata_id: validateId(metadataId) },
      {
        ifMatch,
        ...(signal === undefined ? {} : { signal }),
      },
      { successStatus: 200, requireStrongEntityTag: true },
    );
  }

  async createCommand(
    body: CommandRequest,
    options: CommandCreateOptions,
  ): Promise<WriteResult<CommandJob>> {
    requireCapability(this.#session.descriptor, "commands.async");
    const idempotencyKey = asIdempotencyKey(options?.idempotencyKey as string);
    const signal = options?.signal;
    throwIfAlreadyAborted(signal);
    const snapshot = snapshotCommandRequest(
      body,
      this.#session.descriptor.limits.max_request_body_bytes,
    );
    const value = decodeProtocolValue<CommandRequest>(
      snapshot.value,
      validateCommandRequest,
      "validateCommandRequest",
    );
    validateAdvertisedCommand(this.#session.descriptor, value);
    return this.#writeCommand(
      "createCommand",
      validateCommandJob,
      "validateCommandJob",
      {},
      {
        body: snapshot,
        idempotencyKey,
        ...(signal === undefined ? {} : { signal }),
      },
      { successStatus: 202, requireEntityTag: true, requireLocation: true },
      value,
    );
  }

  async getCommand(
    commandId: string,
    options: ConditionalReadOptions = {},
  ): Promise<ReadResult<CommandJob>> {
    requireCapability(this.#session.descriptor, "commands.async");
    return this.#read(
      "getCommand",
      validateCommandJob,
      "validateCommandJob",
      { command_id: validateId(commandId) },
      {},
      normalizeConditionalOptions(options),
    );
  }

  #historyRead<T>(
    operationName: ReadOperationName,
    validator: ProtocolValidator,
    validatorName: string,
    pathValues: Readonly<Record<string, string>>,
    options: HistoryPageOptions,
    rangeLimit: RangeLimit,
  ): Promise<ReadResult<T>> {
    requireCapability(this.#session.descriptor, "query.history");
    const normalized = this.#normalizeHistoryOptions(options, rangeLimit);
    return this.#read(
      operationName,
      validator,
      validatorName,
      pathValues,
      historyQuery(normalized),
      normalized,
    );
  }

  async #read<T>(
    operationName: ReadOperationName,
    validator: ProtocolValidator,
    validatorName: string,
    pathValues: Readonly<Record<string, string>>,
    query: QueryValues,
    options: ConditionalReadOptions,
    requirements: ReadResponseRequirements = {},
  ): Promise<ReadResult<T>> {
    const descriptor = readOperationDescriptors[operationName];
    const acceptedProtocolVersions = descriptor.versioned
      ? this.#acceptedResponseVersions()
      : undefined;
    const request = buildReadRequest(
      descriptor,
      pathValues,
      query,
      this.#session.protocolVersion,
      options.ifNoneMatch,
      options.signal,
    );
    const transport =
      operationName === "discoverHub"
        ? this.#session.discoveryTransport
        : this.#session.apiTransport;
    const response = await transport.request(request.path, request.init);
    const result = await decodeReadResponse<T>(response, validator, validatorName, options.signal, {
      ...requirements,
      ...(options.ifNoneMatch === undefined ? {} : { ifNoneMatch: options.ifNoneMatch }),
      ...(acceptedProtocolVersions === undefined ? {} : { acceptedProtocolVersions }),
    });
    if (result.kind === "modified") {
      validateReadIdentity(operationName, result.value, pathValues, query, validatorName);
    }
    throwIfAlreadyAborted(options.signal);
    return result;
  }

  async #write<T>(
    operationName: WriteOperationName,
    validator: ProtocolValidator,
    validatorName: string,
    pathValues: Readonly<Record<string, string>>,
    options: WriteRequestOptions,
    requirements: WriteResponseRequirements,
  ): Promise<WriteResult<T>> {
    const acceptedProtocolVersions = this.#acceptedResponseVersions();
    const request = buildWriteRequest(
      writeOperationDescriptors[operationName],
      pathValues,
      this.#session.protocolVersion,
      options,
    );
    const response = await this.#session.apiTransport.request(request.path, request.init);
    const result = await decodeWriteResponse<T>(
      response,
      validator,
      validatorName,
      {
        ...requirements,
        acceptedProtocolVersions,
      },
      options.signal,
    );
    const entity = result.value as Record<string, unknown>;
    if (operationName === "createMetadata") {
      requireIdentity(entity, "vehicle_id", pathValues.vehicle_id, validatorName);
      requireLocationIdentity(
        result.metadata.location,
        "/v1/metadata/",
        entity.metadata_id,
        validatorName,
      );
    } else {
      requireIdentity(entity, "metadata_id", pathValues.metadata_id, validatorName);
    }
    throwIfAlreadyAborted(options.signal);
    return result;
  }

  async #writeCommand<T>(
    operationName: "createCommand",
    validator: ProtocolValidator,
    validatorName: string,
    pathValues: Readonly<Record<string, string>>,
    options: WriteRequestOptions,
    requirements: WriteResponseRequirements,
    expectedCommand: CommandRequest,
  ): Promise<WriteResult<T>> {
    const acceptedProtocolVersions = this.#acceptedResponseVersions();
    let dispatchStarted = false;
    const request = buildWriteRequest(
      writeOperationDescriptors[operationName],
      pathValues,
      this.#session.protocolVersion,
      {
        ...options,
        onDispatch: () => {
          dispatchStarted = true;
        },
      },
    );
    let response: Response;
    try {
      response = await this.#session.apiTransport.request(request.path, request.init);
    } catch (error) {
      if (dispatchStarted) throw new CommandUncertainError();
      throw error;
    }
    try {
      const result = await decodeWriteResponse<T>(
        response,
        validator,
        validatorName,
        { ...requirements, acceptedProtocolVersions },
        options.signal,
      );
      const entity = result.value as Record<string, unknown>;
      for (const key of ["vehicle_id", "command", "command_class"] as const) {
        requireIdentity(entity, key, expectedCommand[key], validatorName);
      }
      requireLocationIdentity(
        result.metadata.location,
        "/v1/commands/",
        entity.command_id,
        validatorName,
      );
      throwIfAlreadyAborted(options.signal);
      return result;
    } catch (error) {
      if (!(error instanceof ProtocolHttpError)) {
        throw new CommandUncertainError();
      }
      throw error;
    }
  }

  #acceptedResponseVersions(): readonly SupportedProtocolVersion[] {
    const versions: readonly SupportedProtocolVersion[] = ["1.0.0", "1.1.0", "1.2.0"];
    return versions.filter((version) => {
      if (versions.indexOf(version) > versions.indexOf(this.#session.protocolVersion)) return false;
      try {
        return negotiateProtocolVersion(this.#session.descriptor, version) === version;
      } catch {
        return false;
      }
    });
  }

  #normalizePageOptions(options: PageReadOptions): PageReadOptions {
    const conditional = normalizeConditionalOptions(options);
    const cursor = normalizeCursor(options.cursor);
    const limit = normalizeLimit(options.limit, this.#session.descriptor.limits.max_page_size);
    return {
      ...conditional,
      ...(cursor === undefined ? {} : { cursor }),
      ...(limit === undefined ? {} : { limit }),
    };
  }

  #normalizeMetadataPageOptions(options: MetadataPageOptions): MetadataPageOptions {
    const page = this.#normalizePageOptions(options);
    if (options.kind !== undefined && typeof options.kind !== "string") {
      throw new InvalidReadOptionsError();
    }
    return {
      ...page,
      ...(options.kind === undefined ? {} : { kind: options.kind }),
    };
  }

  #normalizeHistoryOptions(
    options: HistoryPageOptions,
    rangeLimit: RangeLimit,
  ): HistoryPageOptions {
    const page = this.#normalizePageOptions(options);
    const from = normalizeTimestamp(options.from);
    const to = normalizeTimestamp(options.to);
    if (from !== undefined && to !== undefined) {
      const fromMillis = Date.parse(from);
      const toMillis = Date.parse(to);
      const maximumDays =
        rangeLimit === "dense"
          ? this.#session.descriptor.limits.max_dense_range_days
          : this.#session.descriptor.limits.max_history_range_days;
      if (fromMillis >= toMillis || toMillis - fromMillis > maximumDays * 86_400_000) {
        throw new InvalidReadOptionsError();
      }
    }
    return {
      ...page,
      ...(from === undefined ? {} : { from }),
      ...(to === undefined ? {} : { to }),
    };
  }
}

function requireIdentity(
  entity: Record<string, unknown>,
  key: string,
  expected: unknown,
  validatorName: string,
): void {
  if (entity[key] !== expected) throw new ProtocolValidationError(`${validatorName}.${key}`);
}

function requireLocationIdentity(
  location: string | undefined,
  prefix: string,
  id: unknown,
  validatorName: string,
): void {
  if (typeof id !== "string" || location !== `${prefix}${encodeURIComponent(id)}`) {
    throw new ProtocolValidationError(`${validatorName}.location`);
  }
}

function validateReadIdentity(
  operation: ReadOperationName,
  value: unknown,
  pathValues: Readonly<Record<string, string>>,
  query: QueryValues,
  validatorName: string,
): void {
  const entity = value as Record<string, unknown>;
  const singletonKeys: Partial<Record<ReadOperationName, string>> = {
    getVehicleCurrentState: "vehicle_id",
    getDrive: "drive_id",
    getCharge: "charge_id",
    getMetadata: "metadata_id",
    getCommand: "command_id",
  };
  const singletonKey = singletonKeys[operation];
  if (singletonKey !== undefined) {
    requireIdentity(entity, singletonKey, pathValues[singletonKey], validatorName);
    return;
  }
  const ownerKeys: Partial<Record<ReadOperationName, string>> = {
    listVehicleDrives: "vehicle_id",
    listDrivePositions: "drive_id",
    listVehicleCharges: "vehicle_id",
    listChargeSamples: "charge_id",
    listVehicleStates: "vehicle_id",
    listVehicleUpdates: "vehicle_id",
    listVehicleMetadata: "vehicle_id",
  };
  const ownerKey = ownerKeys[operation];
  const expected = ownerKey === undefined ? undefined : (pathValues[ownerKey] ?? query[ownerKey]);
  if (ownerKey !== undefined && expected !== undefined) {
    for (const item of entity.items as Record<string, unknown>[]) {
      requireIdentity(item, ownerKey, expected, `${validatorName}.items`);
    }
  }
  if (operation === "listDataQuality" && query.vehicle_id !== undefined) {
    for (const item of entity.items as Record<string, unknown>[]) {
      if (item.subject_type === "vehicle") {
        requireIdentity(item, "subject_id", query.vehicle_id, `${validatorName}.items`);
      }
    }
  }
}

function normalizeConditionalOptions(options: ConditionalReadOptions): ConditionalReadOptions {
  const ifNoneMatch = normalizeEntityTag(options.ifNoneMatch);
  return {
    ...(ifNoneMatch === undefined ? {} : { ifNoneMatch }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
  };
}

function normalizeRequestOptions(options: RequestOptions): RequestOptions {
  if (options === null || typeof options !== "object") {
    throw new InvalidReadOptionsError();
  }
  const signal = options.signal;
  return signal === undefined ? {} : { signal };
}

function normalizeEntityTag(value: EntityTag | undefined): EntityTag | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.length > maximumIfNoneMatchLength) {
    throw new InvalidReadOptionsError();
  }
  try {
    return asEntityTag(value);
  } catch {
    throw new InvalidReadOptionsError();
  }
}

function normalizeCursor(value: OpaqueCursor | undefined): OpaqueCursor | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string") throw new InvalidReadOptionsError();
  try {
    return asOpaqueCursor(value);
  } catch {
    throw new InvalidReadOptionsError();
  }
}

function normalizeLimit(value: number | undefined, maximum: number): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 1 || value > maximum) {
    throw new InvalidReadOptionsError();
  }
  return value;
}

function normalizeTimestamp(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  if (
    typeof value !== "string" ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/u.test(value) ||
    !Number.isFinite(Date.parse(value)) ||
    new Date(value).toISOString() !== value
  ) {
    throw new InvalidReadOptionsError();
  }
  return value;
}

function validateId(value: string): string {
  if (
    typeof value !== "string" ||
    value.length < 3 ||
    value.length > 128 ||
    containsControlCharacters(value)
  ) {
    throw new InvalidReadOptionsError();
  }
  return value;
}

function pageQuery(options: PageReadOptions): QueryValues {
  return { cursor: options.cursor, limit: options.limit };
}

function historyQuery(options: HistoryPageOptions): QueryValues {
  return {
    ...pageQuery(options),
    from: options.from,
    to: options.to,
  };
}

function snapshotCommandRequest(body: unknown, maximumBytes: number): JsonRequestSnapshot {
  try {
    return snapshotJsonRequest(body, maximumBytes);
  } catch (error) {
    if (error instanceof InvalidRequestBodyError) {
      throw new ProtocolValidationError("validateCommandRequest");
    }
    throw error;
  }
}

function validateMetadataEntity(value: unknown): boolean {
  return validateMetadataRecord(value) || validateMetadataTombstone(value);
}

function throwIfAlreadyAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted === true) {
    signal.throwIfAborted();
    throw signal.reason;
  }
}
