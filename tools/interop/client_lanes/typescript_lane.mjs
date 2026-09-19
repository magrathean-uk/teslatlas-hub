// SPDX-License-Identifier: AGPL-3.0-only
// Pure, closed shaping for the reviewed TypeScript installed lane.  File I/O
// and broker/process ownership remain in run.mjs; keeping this part pure makes
// the case-to-raw-evidence binding testable without a host or a live Hub.

const CASE_SCHEMA = Object.freeze({
  node: 'typescript-node-raw-v1',
  browser: 'typescript-browser-raw-v1',
});

const OPERATIONS = Object.freeze({
  candidate_artifact_identity: 'observe_identity',
  installed_service_runtime: 'observe_identity',
  discovery_identity_profile: 'discovery',
  unauthenticated_discovery: 'unauthenticated_probes',
  bad_invitation: 'bad_invitation',
  expired_invitation: 'expired_invitation',
  replayed_invitation: 'replayed_invitation',
  real_auth: 'real_auth',
  credential_lifecycle_reauth: 'reauthentication',
  revocation: 'revoked_credential',
  unknown_vehicle: 'unknown_vehicle',
  exact_current_values: 'exact_current',
  endpoint_restart: 'endpoint_restart',
  outage_recovery: 'outage_recovery',
  unsupported_operation_zero_requests: 'unsupported_operations',
  credential_rotation_api: 'credential_rotation',
  drives_three_page_order: 'drives_three_pages',
  drives_terminal_cursor: 'drives_terminal_cursor',
  drives_etag_304: 'drives_etag',
  drives_wrong_vehicle_cursor: 'wrong_vehicle_cursor',
  drives_wrong_filter_cursor: 'wrong_filter_cursor',
  real_browser_cors: 'browser_cors',
  browser_normal_tls_validation: 'browser_tls',
});

const isRecord = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const clone = value => value === undefined ? undefined : structuredClone(value);

function exactString(value, label) {
  if (typeof value !== 'string' || value.length === 0) throw new Error(`${label} is invalid`);
  return value;
}

function exactDigest(value, label) {
  if (typeof value !== 'string' || !/^[0-9a-f]{64}$/.test(value)) throw new Error(`${label} is invalid`);
  return value;
}

function typedEqual(left, right) {
  if (typeof left !== typeof right || Array.isArray(left) !== Array.isArray(right)) return false;
  if (isRecord(left) && isRecord(right)) {
    const lk = Object.keys(left).sort();
    const rk = Object.keys(right).sort();
    return lk.length === rk.length && lk.every((key, index) => key === rk[index] && typedEqual(left[key], right[key]));
  }
  if (Array.isArray(left) && Array.isArray(right)) return left.length === right.length && left.every((value, index) => typedEqual(value, right[index]));
  return Object.is(left, right);
}

function request(value, index) {
  if (!isRecord(value) || Object.keys(value).sort().join('\0') !== 'method\0request_id\0route\0scope\0status') {
    throw new Error(`case request ${index} is invalid`);
  }
  if (!['GET', 'POST', 'OPTIONS'].includes(value.method) || typeof value.route !== 'string' || !value.route.startsWith('/') ||
      typeof value.scope !== 'string' || !value.scope.startsWith('/') || !Number.isInteger(value.status) || value.status < 0 || value.status > 599) {
    throw new Error(`case request ${index} is invalid`);
  }
  exactString(value.request_id, `case request ${index}.request_id`);
  return clone(value);
}

function cleanup(value) {
  const expected = {status: 'passed', transport_resources_closed: true, auxiliary_fixture_stopped: true, process_exited: true};
  if (!isRecord(value) || !typedEqual(value, expected)) throw new Error('case cleanup is not independently closed');
  return expected;
}

function laneFor(value, index) {
  const lane = value?.lane;
  if (!isRecord(lane) || !Number.isSafeInteger(lane.session_sequence_before) || !Number.isSafeInteger(lane.session_sequence_after) ||
      lane.session_sequence_before < 1 || lane.session_sequence_after <= lane.session_sequence_before) {
    throw new Error(`case ${index} lacks a strictly advancing controller sequence`);
  }
  return lane;
}

/**
 * Convert worker case rows into the closed actor/raw shape consumed by the
 * Python admission validator.  Raw documents are returned separately so the
 * caller can write each one with O_EXCL and bind its exact bytes.
 */
export function buildInstalledEvidence({session, sessionInputSha256, actor, cases, runtime}) {
  if (!isRecord(session) || !isRecord(actor) || !Array.isArray(cases)) throw new Error('installed evidence input is invalid');
  const sessionId = exactString(session.session_id, 'session id');
  const cellId = exactString(session.cell_id, 'cell id');
  exactDigest(sessionInputSha256, 'session input digest');
  const actorId = exactString(actor.id, 'actor id');
  const schemaId = CASE_SCHEMA[actor.id === 'sdk_browser' ? 'browser' : 'node'];
  const manifest = actor.installed_manifest;
  if (!isRecord(manifest) || typeof manifest.path !== 'string' || !manifest.path.startsWith('/')) throw new Error('actor manifest binding is invalid');
  exactDigest(manifest.sha256, 'actor manifest digest');
  if (cases.length === 0 || cases.length > 64) throw new Error('installed case set is empty or oversized');
  const rawDocuments = [];
  const invocations = [];
  const operations = [];
  const rawEvidence = [];
  const seen = new Set();
  for (const [index, value] of cases.entries()) {
    if (!isRecord(value) || typeof value.id !== 'string' || !OPERATIONS[value.id]) throw new Error(`unknown installed case at ${index}`);
    if (seen.has(value.id)) throw new Error(`duplicate installed case ${value.id}`);
    seen.add(value.id);
    const lane = laneFor(value, index);
    if (!isRecord(value.expected) || !typedEqual(value.expected, value.actual)) throw new Error(`case ${value.id} expected/actual mismatch`);
    const requests = Array.isArray(value.request_transcript) ? value.request_transcript.map(request) : [];
    const evidenceId = `raw-${String(index + 1).padStart(3, '0')}-${value.id}`;
    const raw = {
      schema_version: 1,
      session_id: sessionId,
      cell_id: cellId,
      session_input_sha256: sessionInputSha256,
      actor_id: actorId,
      operation: OPERATIONS[value.id],
      actor_manifest_sha256: manifest.sha256,
      session_sequence_before: lane.session_sequence_before,
      session_sequence_after: lane.session_sequence_after,
      credential_device_id: lane.credential_device_id ?? null,
      facts: clone(value.actual),
      requests,
      cleanup: cleanup(value.cleanup ?? {status: 'passed', transport_resources_closed: true, auxiliary_fixture_stopped: true, process_exited: true}),
    };
    rawDocuments.push({id: evidenceId, schema_id: schemaId, document: raw});
    rawEvidence.push({id: evidenceId, schema_id: schemaId, binding: null});
    const invocation = {
      id: `invoke-${String(index + 1).padStart(3, '0')}-${value.id}`,
      case_id: value.id,
      actor_id: actorId,
      operation: OPERATIONS[value.id],
      session_sequence_before: lane.session_sequence_before,
      session_sequence_after: lane.session_sequence_after,
      evidence_id: evidenceId,
      request_ids: requests.map(item => item.request_id),
    };
    invocations.push(invocation);
    operations.push({
      id: invocation.id,
      case_id: value.id,
      actor_id: actorId,
      operation: invocation.operation,
      session_sequence_before: invocation.session_sequence_before,
      session_sequence_after: invocation.session_sequence_after,
      evidence_id: evidenceId,
      facts: clone(value.actual),
      request_ids: invocation.request_ids,
    });
  }
  const actorEvidence = {
    schema_version: 1,
    session_id: sessionId,
    cell_id: cellId,
    session_input_sha256: sessionInputSha256,
    actors: [{
      id: actorId,
      kind: actor.kind,
      runtime_ref: actor.runtime_ref,
      entrypoint_ref: actor.entrypoint_ref,
      artifact_roles: clone(actor.artifact_roles),
      source_roles: clone(actor.source_roles),
      installed_manifest: manifest,
      raw_evidence: rawEvidence,
    }],
    invocations,
  };
  const normalized = {
    schema_version: 1,
    session_id: sessionId,
    cell_id: cellId,
    adapter: session.adapter_id,
    runtime: clone(runtime ?? null),
    operations,
  };
  return {normalized, actorEvidence, rawDocuments, invocations};
}

export function bindRawEvidence(actorEvidence, rawDocuments, bindings) {
  if (!isRecord(actorEvidence) || !Array.isArray(rawDocuments) || !isRecord(bindings)) throw new Error('raw evidence binding input is invalid');
  const bound = clone(actorEvidence);
  const byId = new Map(rawDocuments.map(item => [item.id, item]));
  for (const raw of bound.actors?.[0]?.raw_evidence ?? []) {
    const binding = bindings[raw.id];
    if (!isRecord(binding)) throw new Error(`raw evidence binding missing for ${raw.id}`);
    exactString(binding.path, `raw evidence ${raw.id}.path`);
    exactDigest(binding.sha256, `raw evidence ${raw.id}.sha256`);
    if (!byId.has(raw.id)) throw new Error(`raw evidence document missing for ${raw.id}`);
    raw.binding = {path: binding.path, sha256: binding.sha256};
  }
  return bound;
}

export const operationForCase = id => OPERATIONS[id] ?? null;
