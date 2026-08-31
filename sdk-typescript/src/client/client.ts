import {
  validateCharge,
  validateChargePage,
  validateChargeSamplePage,
  validateCurrentState,
  validateDataQualityPage,
  validateDiscovery,
  validateDrive,
  validateDrivePage,
  validatePositionPage,
  validateStatePage,
  validateUpdatePage,
  validateVehiclePage,
} from "../generated/validators.js";
import {
  asEntityTag,
  asOpaqueCursor,
  type EntityTag,
  type OpaqueCursor,
} from "../core/opaque-values.js";
import { containsControlCharacters } from "../core/errors.js";
import {
  buildReadRequest,
  readOperationDescriptors,
  type ReadOperationName,
} from "../http/request-builder.js";
import { decodeReadResponse } from "../http/response-decoder.js";
import type { ProtocolValidator } from "../protocol/validate.js";
import { requireCapability } from "../protocol/capabilities.js";
import type {
  Charge,
  ChargePage,
  ChargeSamplePage,
  CurrentState,
  DataQualityPage,
  Drive,
  DrivePage,
  HubDescriptor,
  PositionPage,
  StatePage,
  UpdatePage,
  VehiclePage,
} from "../protocol/models.js";
import type { ClientSession } from "./types.js";
import type { SupportedProtocolVersion } from "../protocol/negotiation.js";
import {
  InvalidReadOptionsError,
  type ConditionalReadOptions,
  type DataQualityPageOptions,
  type HistoryPageOptions,
  type PageReadOptions,
  type ReadResult,
} from "./operations.js";

type QueryValue = string | number | OpaqueCursor | undefined;
type QueryValues = Readonly<Record<string, QueryValue>>;
type RangeLimit = "history" | "dense";

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

  #read<T>(
    operationName: ReadOperationName,
    validator: ProtocolValidator,
    validatorName: string,
    pathValues: Readonly<Record<string, string>>,
    query: QueryValues,
    options: ConditionalReadOptions,
  ): Promise<ReadResult<T>> {
    const descriptor = readOperationDescriptors[operationName];
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
    return transport
      .request(request.path, request.init)
      .then((response) =>
        decodeReadResponse<T>(response, validator, validatorName, options.signal),
      );
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

function normalizeConditionalOptions(options: ConditionalReadOptions): ConditionalReadOptions {
  const ifNoneMatch = normalizeEntityTag(options.ifNoneMatch);
  return {
    ...(ifNoneMatch === undefined ? {} : { ifNoneMatch }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
  };
}

function normalizeEntityTag(value: EntityTag | undefined): EntityTag | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string") throw new InvalidReadOptionsError();
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
