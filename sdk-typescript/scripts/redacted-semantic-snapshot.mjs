export const SEMANTIC_SNAPSHOT_CANONICALIZATION = "sorted-json-v1";
export const SEMANTIC_SNAPSHOT_PROFILE = "hub-http-v1@1.0.0";

const historyQuery = Object.freeze({
  from_ms: 1_788_565_900_000,
  to_ms: 1_788_566_200_001,
  bounds: "from inclusive, to exclusive",
});
const boundaryQueries = Object.freeze([
  Object.freeze({
    window: "before-equal-start",
    from_ms: 1_788_565_900_000,
    to_ms: 1_788_566_200_000,
  }),
  Object.freeze({
    window: "equal-start-only",
    from_ms: 1_788_566_200_000,
    to_ms: 1_788_566_200_001,
  }),
]);
const nullRule = "null means unavailable, unknown, or not derivable; numeric zero remains zero";
const staleRule =
  "no freshness guarantee beyond observed_at_ms; vehicle-1 timestamp is retained exactly and HA displays telemetry age";
const defaultPlan = Object.freeze({
  vehicleCount: 3,
  historyQuery,
  boundaryQueries,
});

const uuidPattern = /\b[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b/iu;
const forbiddenKeyPattern = /uuid|vehicleid|displayname|address|location|token|cursor|endpoint/iu;
const tokenPattern = /\b[0-9a-f]{64}\b/iu;
const endpointPattern = /\bhttps?:\/\//iu;

export function canonicalizeSortedJson(value) {
  assertCanonicalJsonValue(value, "$", new Set());
  return JSON.stringify(sortJsonValue(value));
}

export async function hashSortedJson(value) {
  const bytes = new TextEncoder().encode(canonicalizeSortedJson(value));
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function createReadOnlyCredentialStore(credential) {
  return Object.freeze({
    load: () => credential,
    save: (_credential) => {
      throw new Error("read-only semantic snapshot attempted to save credentials");
    },
    clear: () => {
      throw new Error("read-only semantic snapshot attempted to clear credentials");
    },
  });
}

export function selectSemanticSnapshotPrimaryVehicle(sourceVehicles, semanticProfile) {
  return resolveSemanticSnapshotPlan(sourceVehicles, semanticProfile).vehicles[0].vehicleId;
}

export async function collectRedactedSemanticSnapshot(client, semanticProfile) {
  if (client === null || typeof client !== "object") throw new Error("Hub client is required");
  const response = await client.vehicles();
  const sourceVehicles = response?.value?.vehicles;
  const plan = resolveSemanticSnapshotPlan(sourceVehicles, semanticProfile);

  const vehicles = [];
  let primaryDrives;
  for (const [index, sourceVehicle] of plan.vehicles.entries()) {
    const label = `vehicle-${index + 1}`;
    const vehicleId = sourceVehicle?.vehicleId;
    if (typeof vehicleId !== "string") throw new Error(`${label} has no vehicle identity`);
    const [currentResponse, drives] = await Promise.all([
      client.current(vehicleId),
      collectDriveWindow(client, vehicleId, plan.historyQuery),
    ]);
    const current = currentResponse?.value;
    if (current === null || typeof current !== "object") {
      throw new Error(`${label} current response is invalid`);
    }
    vehicles.push({
      label,
      current: redactCurrent(current),
      history: {
        query: { ...plan.historyQuery },
        count: drives.length,
        order: drives.map((drive, driveIndex) => redactDrive(drive, driveIndex + 1)),
      },
    });
    if (index === 0) primaryDrives = drives;
  }

  const firstVehicleId = plan.vehicles[0].vehicleId;
  const plannedBoundaries =
    plan.boundaryQueries ?? derivedBoundaryQueries(plan.historyQuery, primaryDrives);
  const history_boundary_checks = [];
  for (const query of plannedBoundaries) {
    const drives = await collectDriveWindow(client, firstVehicleId, query);
    history_boundary_checks.push({ ...query, count: drives.length });
  }

  const snapshot = {
    schema_version: 1,
    profile: SEMANTIC_SNAPSHOT_PROFILE,
    vehicle_order: vehicles.map((vehicle) => vehicle.label),
    vehicles,
    history_boundary_checks,
    null_rule: nullRule,
    stale_rule: staleRule,
  };
  assertRedactedSemanticSnapshot(snapshot);
  return snapshot;
}

export function assertRedactedSemanticSnapshot(value) {
  assertExactObject(value, "$", [
    "schema_version",
    "profile",
    "vehicle_order",
    "vehicles",
    "history_boundary_checks",
    "null_rule",
    "stale_rule",
  ]);
  if (value.schema_version !== 1) throw new Error("semantic snapshot schema_version must be 1");
  if (value.profile !== SEMANTIC_SNAPSHOT_PROFILE)
    throw new Error("semantic snapshot profile differs");
  if (value.null_rule !== nullRule || value.stale_rule !== staleRule) {
    throw new Error("semantic snapshot rules differ");
  }
  if (!Array.isArray(value.vehicle_order) || value.vehicle_order.length === 0) {
    throw new Error("$.vehicle_order must not be empty");
  }
  assertStringArray(value.vehicle_order, "$.vehicle_order", value.vehicle_order.length);
  if (!Array.isArray(value.vehicles) || value.vehicles.length !== value.vehicle_order.length) {
    throw new Error("$.vehicles must match $.vehicle_order");
  }
  const expectedHistoryQuery = value.vehicles[0]?.history?.query;
  assertQueryShape(expectedHistoryQuery, "$.vehicles[0].history.query");
  value.vehicles.forEach((vehicle, index) => {
    const label = `vehicle-${index + 1}`;
    if (value.vehicle_order[index] !== label) throw new Error("$.vehicle_order is not canonical");
    assertVehicle(vehicle, `$.vehicles[${index}]`, label, expectedHistoryQuery);
  });
  if (!Array.isArray(value.history_boundary_checks) || value.history_boundary_checks.length !== 2) {
    throw new Error("$.history_boundary_checks must contain exactly two checks");
  }
  value.history_boundary_checks.forEach((boundary, index) => {
    assertBoundary(boundary, `$.history_boundary_checks[${index}]`);
  });
  assertBoundaryRelationship(value.history_boundary_checks, expectedHistoryQuery);
  assertCanonicalJsonValue(value, "$", new Set());
  assertNoForbiddenSemanticData(value);
}

export function assertNoForbiddenSemanticData(value) {
  visit(value, "$", (entry, path, key) => {
    if (key !== undefined && forbiddenKeyPattern.test(key.replace(/[^a-z0-9]/giu, ""))) {
      throw new Error(`semantic snapshot contains forbidden field at ${path}`);
    }
    if (
      typeof entry === "string" &&
      (uuidPattern.test(entry) || tokenPattern.test(entry) || endpointPattern.test(entry))
    ) {
      throw new Error(`semantic snapshot contains a forbidden string at ${path}`);
    }
  });
}

function assertCanonicalJsonValue(value, path, ancestors) {
  if (value === null || typeof value === "boolean" || typeof value === "string") return;
  if (typeof value === "number") {
    if (!Number.isFinite(value))
      throw new Error(`semantic snapshot has non-finite number at ${path}`);
    return;
  }
  if (typeof value !== "object") {
    throw new Error(`semantic snapshot has non-JSON value at ${path}`);
  }
  if (ancestors.has(value)) throw new Error(`semantic snapshot has a cycle at ${path}`);
  if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype) {
    throw new Error(`semantic snapshot has a non-plain object at ${path}`);
  }
  ancestors.add(value);
  if (Array.isArray(value)) {
    for (const [index, entry] of value.entries()) {
      assertCanonicalJsonValue(entry, `${path}[${index}]`, ancestors);
    }
  } else {
    for (const [key, entry] of Object.entries(value)) {
      assertCanonicalJsonValue(entry, `${path}.${key}`, ancestors);
    }
  }
  ancestors.delete(value);
}

function sortJsonValue(value) {
  if (Array.isArray(value)) return value.map(sortJsonValue);
  if (value === null || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.keys(value)
      .sort()
      .map((key) => [key, sortJsonValue(value[key])]),
  );
}

async function collectDriveWindow(client, vehicleId, query) {
  const items = [];
  const cursors = new Set();
  let cursor;
  for (let pageCount = 0; pageCount < 128; pageCount += 1) {
    const result = await client.drives(vehicleId, {
      fromMs: query.from_ms,
      toMs: query.to_ms,
      limit: 500,
      ...(cursor === undefined ? {} : { cursor }),
    });
    if (result?.kind !== "page" || !Array.isArray(result.value?.items)) {
      throw new Error("semantic history expected a page");
    }
    for (const item of result.value.items) {
      if (
        !Number.isFinite(item?.startDateMs) ||
        item.startDateMs < query.from_ms ||
        item.startDateMs >= query.to_ms
      ) {
        throw new Error("semantic history violated its half-open window");
      }
      items.push(item);
    }
    cursor = result.value.nextCursor;
    if (cursor === null) return items;
    if (typeof cursor !== "string" || cursors.has(cursor)) {
      throw new Error("semantic history cursor is invalid or repeated");
    }
    cursors.add(cursor);
  }
  throw new Error("semantic history exceeded its page bound");
}

function redactCurrent(current) {
  return {
    observed_at_ms: current.observedAtMs,
    state: current.state ?? "unavailable",
    battery_level: measurement(current.batteryLevel, "percent"),
    estimated_range: measurement(current.estBatteryRangeKm, "km"),
    odometer: measurement(current.odometer, "km"),
    speed: measurement(current.speed, "km/h"),
    inside_temperature: measurement(current.insideTemp, "degrees Celsius"),
    outside_temperature: measurement(current.outsideTemp, "degrees Celsius"),
    charger_power: measurement(current.chargerPower, "kW"),
    locked: current.locked,
  };
}

function redactDrive(drive, ordinal) {
  return {
    ordinal,
    start_date_ms: drive.startDateMs,
    end_date_ms: drive.endDateMs,
    distance: measurement(drive.distanceKm, "km"),
    duration: measurement(drive.durationMin, "minutes"),
  };
}

function measurement(value, unit) {
  return { value, unit };
}

function assertVehicle(value, path, label, expectedHistoryQuery) {
  assertExactObject(value, path, ["label", "current", "history"]);
  if (value.label !== label) throw new Error(`${path}.label differs`);
  assertCurrent(value.current, `${path}.current`);
  assertHistory(value.history, `${path}.history`, expectedHistoryQuery);
}

function assertCurrent(value, path) {
  assertExactObject(value, path, [
    "observed_at_ms",
    "state",
    "battery_level",
    "estimated_range",
    "odometer",
    "speed",
    "inside_temperature",
    "outside_temperature",
    "charger_power",
    "locked",
  ]);
  assertNullableFinite(value.observed_at_ms, `${path}.observed_at_ms`);
  if (typeof value.state !== "string") throw new Error(`${path}.state must be a string`);
  assertMeasurement(value.battery_level, `${path}.battery_level`, "percent");
  assertMeasurement(value.estimated_range, `${path}.estimated_range`, "km");
  assertMeasurement(value.odometer, `${path}.odometer`, "km");
  assertMeasurement(value.speed, `${path}.speed`, "km/h");
  assertMeasurement(value.inside_temperature, `${path}.inside_temperature`, "degrees Celsius");
  assertMeasurement(value.outside_temperature, `${path}.outside_temperature`, "degrees Celsius");
  assertMeasurement(value.charger_power, `${path}.charger_power`, "kW");
  if (value.locked !== null && typeof value.locked !== "boolean") {
    throw new Error(`${path}.locked must be boolean or null`);
  }
}

function assertHistory(value, path, expectedHistoryQuery) {
  assertExactObject(value, path, ["query", "count", "order"]);
  assertQuery(value.query, `${path}.query`, expectedHistoryQuery);
  assertNonNegativeInteger(value.count, `${path}.count`);
  if (!Array.isArray(value.order) || value.order.length !== value.count) {
    throw new Error(`${path}.order length differs from count`);
  }
  value.order.forEach((item, index) => {
    const itemPath = `${path}.order[${index}]`;
    assertExactObject(item, itemPath, [
      "ordinal",
      "start_date_ms",
      "end_date_ms",
      "distance",
      "duration",
    ]);
    if (item.ordinal !== index + 1) throw new Error(`${itemPath}.ordinal differs`);
    assertFinite(item.start_date_ms, `${itemPath}.start_date_ms`);
    assertFinite(item.end_date_ms, `${itemPath}.end_date_ms`);
    if (
      item.start_date_ms < expectedHistoryQuery.from_ms ||
      item.start_date_ms >= expectedHistoryQuery.to_ms
    ) {
      throw new Error(`${itemPath}.start_date_ms is outside the half-open query`);
    }
    if (index > 0 && value.order[index - 1].start_date_ms < item.start_date_ms) {
      throw new Error(`${path}.order is not descending by start_date_ms`);
    }
    assertMeasurement(item.distance, `${itemPath}.distance`, "km");
    assertMeasurement(item.duration, `${itemPath}.duration`, "minutes");
  });
}

function assertQuery(value, path, expected) {
  assertQueryShape(value, path);
  if (value.from_ms !== expected.from_ms || value.to_ms !== expected.to_ms) {
    throw new Error(`${path} differs from the canonical half-open query`);
  }
}

function assertQueryShape(value, path) {
  assertExactObject(value, path, ["from_ms", "to_ms", "bounds"]);
  assertSafeInteger(value.from_ms, `${path}.from_ms`);
  assertSafeInteger(value.to_ms, `${path}.to_ms`);
  if (value.from_ms >= value.to_ms || value.bounds !== historyQuery.bounds) {
    throw new Error(`${path} is not a valid half-open query`);
  }
}

function assertBoundary(value, path) {
  assertExactObject(value, path, ["window", "from_ms", "to_ms", "count"]);
  if (typeof value.window !== "string") throw new Error(`${path}.window must be a string`);
  assertSafeInteger(value.from_ms, `${path}.from_ms`);
  assertSafeInteger(value.to_ms, `${path}.to_ms`);
  if (value.from_ms >= value.to_ms) throw new Error(`${path} is not a half-open window`);
  assertNonNegativeInteger(value.count, `${path}.count`);
}

function assertBoundaryRelationship(boundaries, query) {
  const [before, latest] = boundaries;
  if (
    before.window !== "before-equal-start" ||
    latest.window !== "equal-start-only" ||
    before.from_ms !== query.from_ms ||
    before.to_ms !== latest.from_ms ||
    latest.to_ms !== latest.from_ms + 1 ||
    latest.to_ms > query.to_ms
  ) {
    throw new Error("$.history_boundary_checks do not partition the derived latest start");
  }
}

function assertMeasurement(value, path, unit) {
  assertExactObject(value, path, ["value", "unit"]);
  assertNullableFinite(value.value, `${path}.value`);
  if (value.unit !== unit) throw new Error(`${path}.unit differs`);
}

function assertExactObject(value, path, keys) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${path} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error(`${path} has an unexpected shape`);
  }
}

function assertStringArray(value, path, length) {
  if (
    !Array.isArray(value) ||
    value.length !== length ||
    value.some((item) => typeof item !== "string")
  ) {
    throw new Error(`${path} must contain exactly ${length} strings`);
  }
}

function assertNonNegativeInteger(value, path) {
  if (!Number.isSafeInteger(value) || value < 0)
    throw new Error(`${path} must be a non-negative integer`);
}

function assertSafeInteger(value, path) {
  if (!Number.isSafeInteger(value)) throw new Error(`${path} must be a safe integer`);
}

function assertNullableFinite(value, path) {
  if (value !== null) assertFinite(value, path);
}

function assertFinite(value, path) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`${path} must be a finite number`);
  }
}

function visit(value, path, operation, key) {
  operation(value, path, key);
  if (Array.isArray(value)) {
    value.forEach((entry, index) => {
      visit(entry, `${path}[${index}]`, operation);
    });
  } else if (value !== null && typeof value === "object") {
    for (const [childKey, entry] of Object.entries(value)) {
      visit(entry, `${path}.${childKey}`, operation, childKey);
    }
  }
}

function resolveSemanticSnapshotPlan(sourceVehicles, semanticProfile) {
  const profile = normalizeSemanticProfile(semanticProfile);
  if (!Array.isArray(sourceVehicles) || sourceVehicles.length !== profile.vehicleCount) {
    throw new Error(`semantic snapshot requires exactly ${profile.vehicleCount} ordered vehicles`);
  }
  const vehicles = [...sourceVehicles];
  if (profile.primaryVehicleId !== undefined) {
    const primaryIndex = vehicles.findIndex(
      (vehicle) => vehicle?.vehicleId === profile.primaryVehicleId,
    );
    if (primaryIndex < 0) throw new Error("semantic snapshot primary vehicle is absent");
    const [primary] = vehicles.splice(primaryIndex, 1);
    vehicles.unshift(primary);
  }
  return { ...profile, vehicles };
}

function normalizeSemanticProfile(value) {
  if (value === undefined) return defaultPlan;
  assertExactObject(value, "semantic profile", [
    "schemaVersion",
    "primaryVehicleId",
    "vehicleCount",
    "history",
    "boundaryMode",
  ]);
  if (value.schemaVersion !== 1) throw new Error("semantic profile schemaVersion must be 1");
  if (typeof value.primaryVehicleId !== "string" || !uuidPattern.test(value.primaryVehicleId)) {
    throw new Error("semantic profile primaryVehicleId must be a UUID");
  }
  if (
    !Number.isSafeInteger(value.vehicleCount) ||
    value.vehicleCount < 1 ||
    value.vehicleCount > 500
  ) {
    throw new Error("semantic profile vehicleCount must be an integer from 1 through 500");
  }
  assertExactObject(value.history, "semantic profile.history", ["fromMs", "toMs"]);
  assertSafeInteger(value.history.fromMs, "semantic profile.history.fromMs");
  assertSafeInteger(value.history.toMs, "semantic profile.history.toMs");
  if (value.history.fromMs >= value.history.toMs) {
    throw new Error("semantic profile history must be a non-empty half-open window");
  }
  if (value.boundaryMode !== "derived-latest-drive") {
    throw new Error('semantic profile boundaryMode must be "derived-latest-drive"');
  }
  return {
    primaryVehicleId: value.primaryVehicleId,
    vehicleCount: value.vehicleCount,
    historyQuery: {
      from_ms: value.history.fromMs,
      to_ms: value.history.toMs,
      bounds: historyQuery.bounds,
    },
    boundaryQueries: undefined,
  };
}

function derivedBoundaryQueries(query, primaryDrives) {
  if (!Array.isArray(primaryDrives) || primaryDrives.length === 0) {
    throw new Error("semantic snapshot cannot derive boundaries without a primary drive");
  }
  const latestStart = primaryDrives[0].startDateMs;
  if (
    !Number.isSafeInteger(latestStart) ||
    latestStart <= query.from_ms ||
    latestStart >= query.to_ms
  ) {
    throw new Error("semantic snapshot latest primary drive cannot form boundary windows");
  }
  return [
    {
      window: "before-equal-start",
      from_ms: query.from_ms,
      to_ms: latestStart,
    },
    {
      window: "equal-start-only",
      from_ms: latestStart,
      to_ms: latestStart + 1,
    },
  ];
}
