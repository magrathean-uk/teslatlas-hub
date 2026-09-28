#!/usr/bin/env node
/*
 * Installed Viewer matrix coordinator.
 *
 * This entrypoint is deliberately separate from the public CLI and from the
 * managed live lane.  It consumes the shared runner's private SessionInput,
 * owns one fixed broker attachment, and runs the already-built installed
 * Playwright smoke.  It never builds source, accepts a command from JSON, or
 * writes a success receipt without the runner's close acknowledgement.
 */

import { constants } from 'node:fs';
import { mkdir, open, stat } from 'node:fs/promises';
import net from 'node:net';
import { createHash, X509Certificate } from 'node:crypto';
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(here, '..');
const FRAME_BYTES = 1_048_576;
const EVIDENCE_BYTES = 8_388_608;
const CASES = [
  ['candidate_artifact_identity', 'identity'],
  ['installed_service_runtime', 'identity'],
  ['discovery_identity_profile', 'http'],
  ['unauthenticated_discovery', 'http'],
  ['bad_invitation', 'http'],
  ['expired_invitation', 'zero_request'],
  ['replayed_invitation', 'http'],
  ['real_auth', 'http'],
  ['credential_lifecycle_reauth', 'http'],
  ['revocation', 'http'],
  ['unknown_vehicle', 'http'],
  ['exact_current_values', 'http'],
  ['endpoint_restart', 'http'],
  ['outage_recovery', 'http'],
  ['unsupported_operation_zero_requests', 'zero_request'],
  ['credential_rotation_api', 'http'],
  ['drives_three_page_order', 'http'],
  ['drives_terminal_cursor', 'http'],
  ['drives_etag_304', 'http'],
  ['drives_wrong_vehicle_cursor', 'zero_request'],
  ['drives_wrong_filter_cursor', 'zero_request'],
  ['built_viewer_packed_sdk', 'identity'],
  ['real_browser_cors', 'http'],
  ['browser_normal_tls_validation', 'http'],
];
const ACTOR_SPECS = [
  {
    id: 'viewer_ui',
    kind: 'built_viewer_ui',
    execution: 'browser_worker',
    runtime_ref: 'viewer_browser',
    entrypoint_ref: 'viewer_installed_playwright',
    artifact_roles: ['viewer_package_tarball'],
    source_roles: ['viewer_source'],
  },
  {
    id: 'viewer_sdk_contract',
    kind: 'packed_sdk_dependency',
    execution: 'local_worker',
    runtime_ref: 'viewer_browser',
    entrypoint_ref: 'viewer_installed_playwright_sdk_dependency',
    artifact_roles: ['typescript_sdk_tarball'],
    source_roles: ['typescript_sdk_source'],
  },
];
const HEX64 = /^[0-9a-f]{64}$/u;
const UUID4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
const CELL = /^viewer__(macos_arm64|debian13_amd64|debian13_arm64)$/u;
const LOOPBACK_HTTPS_ORIGIN = /^https:\/\/(127\.0\.0\.1|localhost):([1-9][0-9]{0,4})$/u;

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const canonicalize = (value) => {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonicalize(value[key])]));
  }
  return value;
};
const jsonBytes = (value) => Buffer.from(`${JSON.stringify(canonicalize(value))}\n`, 'utf8');
const strictJson = (bytes) => {
  if (!(bytes instanceof Uint8Array) && !Buffer.isBuffer(bytes)) throw new Error('JSON input must be bytes');
  let source;
  try { source = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch (error) { throw new Error('JSON input is not UTF-8', { cause: error }); }
  let index = 0;
  const fail = () => { throw new Error('invalid JSON'); };
  const whitespace = () => { while (index < source.length && ' \t\r\n'.includes(source[index])) index += 1; };
  const string = () => {
    if (source[index] !== '"') return fail();
    const start = index;
    index += 1;
    let closed = false;
    while (index < source.length) {
      const character = source[index++];
      if (character === '\\') { if (index >= source.length) return fail(); index += 1; continue; }
      if (character === '"') { closed = true; break; }
      if (character < ' ') return fail();
    }
    if (!closed) return fail();
    try { return JSON.parse(source.slice(start, index)); } catch (error) { throw new Error('invalid JSON string', { cause: error }); }
  };
  const value = () => {
    whitespace();
    const character = source[index];
    if (character === '"') return string();
    if (character === '{') {
      index += 1;
      const object = {};
      whitespace();
      if (source[index] === '}') { index += 1; return object; }
      while (true) {
        whitespace();
        const key = string();
        if (Object.hasOwn(object, key)) throw new Error('duplicate JSON key');
        whitespace();
        if (source[index++] !== ':') return fail();
        Object.defineProperty(object, key, { value: value(), enumerable: true, writable: true, configurable: true });
        whitespace();
        if (source[index] === ',') { index += 1; continue; }
        if (source[index++] !== '}') return fail();
        return object;
      }
    }
    if (character === '[') {
      index += 1;
      const array = [];
      whitespace();
      if (source[index] === ']') { index += 1; return array; }
      while (true) {
        array.push(value());
        whitespace();
        if (source[index] === ',') { index += 1; continue; }
        if (source[index++] !== ']') return fail();
        return array;
      }
    }
    for (const [literal, result] of [['true', true], ['false', false], ['null', null]]) {
      if (source.startsWith(literal, index)) { index += literal.length; return result; }
    }
    const match = source.slice(index).match(/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/u);
    if (match) {
      index += match[0].length;
      const number = Number(match[0]);
      if (!Number.isFinite(number)) return fail();
      return number;
    }
    return fail();
  };
  const result = value();
  whitespace();
  if (index !== source.length) return fail();
  return result;
};
const exact = (value, keys, label) => {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error(`${label} fields are invalid`);
  }
  return value;
};
const text = (value, label, { max = 4096, pattern } = {}) => {
  if (typeof value !== 'string' || value.length < 1 || value.length > max || (pattern && !pattern.test(value))) {
    throw new Error(`${label} is invalid`);
  }
  if (/[\0\r\n]/u.test(value)) throw new Error(`${label} contains a forbidden control`);
  return value;
};
const digest = (value, label) => text(value, label, { max: 64, pattern: HEX64 });
const absolute = (value, label) => {
  text(value, label, { max: 2048, pattern: /^\//u });
  if (path.resolve(value) !== value || value.split('/').includes('..')) throw new Error(`${label} is not canonical`);
  return value;
};
const uuid = (value, label) => text(value, label, { max: 36, pattern: UUID4 });
const token = (value, label) => text(value, label, { max: 128, pattern: /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/u });

function validateInvitation(value, label) {
  exact(value, ['pairingId', 'secret', 'expiresAtMs', 'endpoint', 'tlsPin', 'pairingUri'], label);
  uuid(value.pairingId, `${label}.pairingId`);
  text(value.secret, `${label}.secret`, { max: 512 });
  if (!Number.isSafeInteger(value.expiresAtMs) || value.expiresAtMs <= 0) throw new Error(`${label}.expiresAtMs is invalid`);
  if (value.endpoint !== 'https://127.0.0.1:18480') throw new Error(`${label}.endpoint is invalid`);
  digest(value.tlsPin, `${label}.tlsPin`);
  text(value.pairingUri, `${label}.pairingUri`, { max: 4096 });
  let parsed;
  try { parsed = new URL(value.pairingUri); } catch (error) { throw new Error(`${label}.pairingUri is invalid`, { cause: error }); }
  const query = [...parsed.searchParams.entries()];
  const expected = [
    ['endpoint', value.endpoint],
    ['pairing_id', value.pairingId],
    ['secret', value.secret],
    ['tls_pin', value.tlsPin],
  ];
  if (parsed.protocol !== 'teslatlas-hub:' || parsed.hostname !== 'pair' || parsed.port || parsed.username || parsed.password || parsed.pathname || parsed.hash || query.length !== expected.length || query.some((item, index) => item[0] !== expected[index][0] || item[1] !== expected[index][1])) {
    throw new Error(`${label}.pairingUri binding is invalid`);
  }
  return value;
}

function validateDescriptor(value) {
  exact(value, ['status', 'provenance', 'endpoint', 'hub_id', 'hub_pid', 'hub_started_at', 'service_generation', 'binary_sha256', 'seed_binary_sha256', 'profile_id', 'profile_path', 'profile_sha256', 'scenario_path', 'scenario_sha256', 'certificate_path'], 'broker descriptor');
  if (value.status !== 'ready' || value.provenance !== 'installed-package-service' || value.endpoint !== 'https://127.0.0.1:18480') throw new Error('broker descriptor identity is invalid');
  uuid(value.hub_id, 'broker descriptor.hub_id');
  if (!Number.isSafeInteger(value.hub_pid) || value.hub_pid < 1) throw new Error('broker descriptor.hub_pid is invalid');
  text(value.hub_started_at, 'broker descriptor.hub_started_at');
  text(value.service_generation, 'broker descriptor.service_generation');
  for (const key of ['binary_sha256', 'seed_binary_sha256', 'profile_sha256', 'scenario_sha256']) digest(value[key], `broker descriptor.${key}`);
  if (value.profile_id !== 'hub-http-v1@1.0.0') throw new Error('broker descriptor.profile_id is invalid');
  for (const key of ['profile_path', 'scenario_path', 'certificate_path']) absolute(value[key], `broker descriptor.${key}`);
  return value;
}

function validateRunningResult(value, sessionId, sequence, requestChallenge) {
  exact(value, ['descriptor', 'proof', 'invitation', 'expired_invitation', 'events'], 'broker running result');
  validateDescriptor(value.descriptor);
  validateInvitation(value.invitation, 'broker result.invitation');
  validateInvitation(value.expired_invitation, 'broker result.expired_invitation');
  if (value.invitation.pairingId === value.expired_invitation.pairingId || value.invitation.tlsPin !== value.expired_invitation.tlsPin) throw new Error('broker invitation identity is invalid');
  if (value.invitation.expiresAtMs <= Date.now() || value.expired_invitation.expiresAtMs >= Date.now()) throw new Error('broker invitation lifetime is invalid');
  const proof = value.proof;
  if (!proof || typeof proof !== 'object' || Array.isArray(proof) || proof.schema_version !== 1 || proof.status !== 'verified' || proof.session_id !== sessionId || proof.sequence !== sequence || proof.challenge !== requestChallenge) throw new Error('broker proof identity is invalid');
  digest(proof.challenge, 'broker proof.challenge');
  if (!Array.isArray(value.events) || value.events.length > 512) throw new Error('broker events are invalid');
  return value;
}

async function ownerFile(pathname, label, maximum = FRAME_BYTES, privateOnly = true) {
  absolute(pathname, label);
  const metadata = await stat(pathname, { bigint: false });
  if (!metadata.isFile() || metadata.uid !== process.getuid() || metadata.nlink !== 1 || (privateOnly && (metadata.mode & 0o077) !== 0) || metadata.size > maximum) {
    throw new Error(`${label} must be an owner-only regular file`);
  }
  const handle = await open(pathname, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | constants.O_NONBLOCK);
  try {
    const opened = await handle.stat();
    if (!opened.isFile() || opened.uid !== process.getuid() || opened.nlink !== 1 || (privateOnly && (opened.mode & 0o077) !== 0) || opened.size > maximum || opened.dev !== metadata.dev || opened.ino !== metadata.ino) {
      throw new Error(`${label} changed while opening`);
    }
    const bytes = await handle.readFile();
    const after = await handle.stat();
    if (after.size !== opened.size || after.mtimeMs !== opened.mtimeMs || after.ctimeMs !== opened.ctimeMs || after.dev !== opened.dev || after.ino !== opened.ino || bytes.length !== opened.size) {
      throw new Error(`${label} changed while reading`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

async function privateJson(pathname, label, maximum = FRAME_BYTES) {
  const bytes = await ownerFile(pathname, label, maximum);
  let value;
  try {
    value = strictJson(bytes);
  } catch (error) {
    throw new Error(`${label} is not JSON`, { cause: error });
  }
  return { value, bytes };
}

async function binding(pathname, label, maximum = EVIDENCE_BYTES) {
  const bytes = await ownerFile(pathname, label, maximum);
  return { path: pathname, sha256: sha256(bytes) };
}

async function exclusiveJson(pathname, value, label) {
  absolute(pathname, label);
  const bytes = jsonBytes(value);
  if (bytes.length > EVIDENCE_BYTES) throw new Error(`${label} exceeds its byte bound`);
  const handle = await open(
    pathname,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0),
    0o600,
  );
  try {
    await handle.writeFile(bytes);
    await handle.sync();
  } finally {
    await handle.close();
  }
  return { path: pathname, sha256: sha256(bytes) };
}

function staged(value, label) {
  exact(value, ['id', 'root', 'local'], label);
  token(value.id, `${label}.id`);
  for (const [name, item] of Object.entries({ root: value.root, local: value.local })) {
    exact(item, ['path', 'sha256'], `${label}.${name}`);
    absolute(item.path, `${label}.${name}.path`);
    digest(item.sha256, `${label}.${name}.sha256`);
  }
  if (value.root.sha256 !== value.local.sha256) throw new Error(`${label} root/local digest differs`);
  return value;
}

function loopbackHttpsOrigin(value, label) {
  const origin = text(value, label, { max: 128, pattern: LOOPBACK_HTTPS_ORIGIN });
  const match = LOOPBACK_HTTPS_ORIGIN.exec(origin);
  if (!match || Number(match[2]) > 65_535) throw new Error(`${label} is not a canonical loopback HTTPS origin`);
  return origin;
}

export function validateViewerWire(value) {
  exact(value, ['page', 'hub', 'trusted_ca', 'browser', 'reservations'], 'Viewer wire');
  const page = exact(value.page, ['origin', 'server_authority', 'artifact_role'], 'Viewer page');
  const pageOrigin = loopbackHttpsOrigin(page.origin, 'Viewer page origin');
  if (page.server_authority !== 'runner-owned-installed-viewer' || page.artifact_role !== 'viewer_package_tarball') {
    throw new Error('Viewer page authority is invalid');
  }

  const hub = exact(value.hub, ['public_origin', 'cors_allowed_origin', 'cross_origin'], 'Viewer Hub');
  const hubOrigin = loopbackHttpsOrigin(hub.public_origin, 'Viewer Hub public origin');
  if (hub.cors_allowed_origin !== pageOrigin || hub.cross_origin !== true || hubOrigin === pageOrigin) {
    throw new Error('Viewer Hub CORS binding is invalid');
  }

  const trustedCa = exact(value.trusted_ca, ['certificate', 'certificate_der_sha256'], 'Viewer trusted CA');
  staged(trustedCa.certificate, 'Viewer trusted CA certificate');
  if (trustedCa.certificate.id !== 'viewer_trusted_ca' || !HEX64.test(trustedCa.certificate_der_sha256)) {
    throw new Error('Viewer trusted CA binding is invalid');
  }

  const browser = exact(value.browser, ['authority', 'engine', 'version', 'executable'], 'Viewer browser');
  if (browser.authority !== 'runner-bound-executable' || browser.engine !== 'chromium' || !/^[0-9]+(?:\.[0-9]+){1,3}$/u.test(browser.version)) {
    throw new Error('Viewer browser identity is invalid');
  }
  const executable = exact(browser.executable, ['path', 'sha256'], 'Viewer browser executable');
  absolute(executable.path, 'Viewer browser executable.path');
  digest(executable.sha256, 'Viewer browser executable.sha256');

  const reservations = exact(value.reservations, ['raw_evidence_dir', 'browser_log', 'close_record', 'supplement'], 'Viewer reservations');
  const paths = Object.entries(reservations).map(([key, pathname]) => absolute(pathname, `Viewer reservation.${key}`));
  if (new Set(paths).size !== paths.length || new Set(paths.map((pathname) => path.dirname(pathname))).size !== 1) {
    throw new Error('Viewer reservations are not distinct siblings');
  }
  return value;
}

function validateSession(session) {
  exact(session, [
    'schema_version', 'kind', 'run_id', 'cell_id', 'adapter_id', 'client_id',
    'session_id', 'instance_nonce', 'header', 'case_contract', 'host_session',
    'broker', 'inputs', 'actors', 'outputs', 'viewer', 'bounds',
  ], 'session input');
  if (session.schema_version !== 1 || session.kind !== 'matrix-adapter-session') throw new Error('session identity is invalid');
  token(session.run_id, 'session.run_id');
  if (session.adapter_id !== 'viewer' || session.client_id !== 'viewer' || !CELL.test(session.cell_id)) throw new Error('session client identity is invalid');
  uuid(session.session_id, 'session.session_id');
  digest(session.instance_nonce, 'session.instance_nonce');
  staged(session.header, 'session.header');
  staged(session.case_contract, 'session.case_contract');
  const host = exact(session.host_session, ['schema_version', 'kind', 'broker_socket', 'session_id', 'registration_sha256'], 'session.host_session');
  if (host.schema_version !== 1 || host.kind !== 'installed-host') throw new Error('host session identity is invalid');
  absolute(host.broker_socket, 'session.host_session.broker_socket');
  uuid(host.session_id, 'session.host_session.session_id');
  digest(host.registration_sha256, 'session.host_session.registration_sha256');
  if (host.session_id !== session.session_id) throw new Error('host/session identity differs');
  const broker = exact(session.broker, ['kind', 'socket_path'], 'session.broker');
  if (broker.kind !== 'unix') throw new Error('Viewer requires a Unix broker');
  absolute(broker.socket_path, 'session.broker.socket_path');
  const inputs = exact(session.inputs, ['profile_manifest', 'profile_members', 'scenario', 'certificate', 'certificate_der_sha256', 'product_inputs'], 'session.inputs');
  staged(inputs.profile_manifest, 'profile manifest');
  if (!Array.isArray(inputs.profile_members) || inputs.profile_members.length !== 18) throw new Error('profile members are incomplete');
  inputs.profile_members.forEach((item, index) => staged(item, `profile member ${index}`));
  staged(inputs.scenario, 'scenario');
  staged(inputs.certificate, 'certificate');
  digest(inputs.certificate_der_sha256, 'certificate DER digest');
  if (!Array.isArray(inputs.product_inputs) || inputs.product_inputs.length !== 2) throw new Error('Viewer product inputs are incomplete');
  const productRoles = new Set();
  for (const [index, product] of inputs.product_inputs.entries()) {
    exact(product, ['artifact_role', 'staged', 'installed_manifest', 'local_root'], `product input ${index}`);
    if (typeof product.artifact_role !== 'string') throw new Error(`product input ${index} role is invalid`);
    if (productRoles.has(product.artifact_role)) throw new Error('product input roles are duplicated');
    productRoles.add(product.artifact_role);
    staged(product.staged, `product input ${index}.staged`);
    if (product.installed_manifest !== null) staged(product.installed_manifest, `product input ${index}.installed_manifest`);
    if (product.local_root !== null) absolute(product.local_root, `product input ${index}.local_root`);
    if ((product.installed_manifest === null) !== (product.local_root === null)) throw new Error('product install provenance is incomplete');
  }
  if (!productRoles.has('viewer_package_tarball') || !productRoles.has('typescript_sdk_tarball')) throw new Error('Viewer product roles are incomplete');
  if (!Array.isArray(session.actors) || session.actors.length !== ACTOR_SPECS.length) throw new Error('Viewer actors are incomplete');
  for (const [index, expected] of ACTOR_SPECS.entries()) {
    const actor = session.actors[index];
    exact(actor, ['id', 'kind', 'execution', 'runtime_ref', 'artifact_roles', 'source_roles', 'entrypoint_ref', 'input_manifest', 'phase_contract'], `actor ${index}`);
    for (const key of ['id', 'kind', 'execution', 'runtime_ref', 'entrypoint_ref']) if (actor[key] !== expected[key]) throw new Error(`actor ${index}.${key} is not fixed`);
    if (JSON.stringify(actor.artifact_roles) !== JSON.stringify(expected.artifact_roles) || JSON.stringify(actor.source_roles) !== JSON.stringify(expected.source_roles)) throw new Error(`actor ${index} roles are not fixed`);
    staged(actor.input_manifest, `actor ${index}.input_manifest`);
    if (actor.phase_contract !== null) staged(actor.phase_contract, `actor ${index}.phase_contract`);
  }
  const outputs = exact(session.outputs, ['normalized', 'actor_evidence', 'coordination_dir', 'framework_log'], 'session.outputs');
  for (const [key, value] of Object.entries(outputs)) absolute(value, `session.outputs.${key}`);
  if (new Set(Object.values(outputs)).size !== Object.values(outputs).length) throw new Error('session output paths overlap');
  validateViewerWire(session.viewer);
  const bounds = exact(session.bounds, ['cell_timeout_ms', 'cleanup_timeout_ms', 'frame_bytes', 'evidence_bytes', 'framework_log_bytes'], 'session.bounds');
  if (!Number.isSafeInteger(bounds.cell_timeout_ms) || bounds.cell_timeout_ms < 1 || bounds.cell_timeout_ms > 3_600_000 || bounds.cleanup_timeout_ms !== 45_000 || bounds.frame_bytes !== FRAME_BYTES || bounds.evidence_bytes !== EVIDENCE_BYTES || bounds.framework_log_bytes !== EVIDENCE_BYTES) throw new Error('session bounds are invalid');
  return session;
}

function safeChildEnvironment(values) {
  const allow = ['PATH', 'HOME', 'USER', 'TMPDIR', 'NODE_EXTRA_CA_CERTS', 'PLAYWRIGHT_BROWSERS_PATH', 'CI', 'DISPLAY', 'XDG_RUNTIME_DIR'];
  const env = {};
  for (const key of allow) if (typeof process.env[key] === 'string') env[key] = process.env[key];
  Object.assign(env, values);
  return env;
}

async function boundViewerRuntime(viewer) {
  const certificate = viewer.trusted_ca.certificate;
  const root = await ownerFile(certificate.root.path, 'Viewer trusted CA root');
  const local = await ownerFile(certificate.local.path, 'Viewer trusted CA local');
  if (sha256(root) !== certificate.root.sha256 || sha256(local) !== certificate.local.sha256 || !root.equals(local)) {
    throw new Error('Viewer trusted CA binding changed');
  }
  let parsedCertificate;
  try {
    parsedCertificate = new X509Certificate(root);
  } catch (error) {
    throw new Error('Viewer trusted CA is not a certificate', { cause: error });
  }
  if (sha256(parsedCertificate.raw) !== viewer.trusted_ca.certificate_der_sha256) {
    throw new Error('Viewer trusted CA DER binding changed');
  }

  const executablePath = viewer.browser.executable.path;
  const executableMetadata = await stat(executablePath);
  if (!executableMetadata.isFile() || (executableMetadata.mode & 0o100) === 0) {
    throw new Error('Viewer browser executable is not executable');
  }
  const executable = await ownerFile(executablePath, 'Viewer browser executable', 1_073_741_824, false);
  if (sha256(executable) !== viewer.browser.executable.sha256) {
    throw new Error('Viewer browser executable binding changed');
  }
  return { certificatePath: certificate.local.path, executablePath };
}

class Broker {
  constructor(socketPath, sessionId) {
    this.socketPath = socketPath;
    this.sessionId = sessionId;
    this.socket = null;
    this.buffer = Buffer.alloc(0);
    this.challenge = null;
    this.sequence = 0;
  }

  async open(deadline) {
    this.socket = await new Promise((resolve, reject) => {
      const socket = net.createConnection(this.socketPath);
      const timer = setTimeout(() => { socket.destroy(); reject(new Error('broker greeting timed out')); }, Math.max(1, deadline - Date.now()));
      socket.once('connect', () => { clearTimeout(timer); resolve(socket); });
      socket.once('error', (error) => { clearTimeout(timer); reject(error); });
    });
    const greeting = await this.frame(deadline);
    exact(greeting, ['schema_version', 'type', 'session_id', 'sequence', 'challenge'], 'broker greeting');
    if (greeting.schema_version !== 1 || greeting.type !== 'challenge' || greeting.session_id !== this.sessionId || greeting.sequence !== 0) throw new Error('broker greeting identity is invalid');
    digest(greeting.challenge, 'broker greeting challenge');
    this.challenge = greeting.challenge;
    return this;
  }

  async request(op, extra = {}, deadline) {
    if (!this.socket || !this.challenge) throw new Error('broker is not open');
    if (!['verify', 'pair', 'revoke', 'start', 'stop'].includes(op)) throw new Error('broker operation is not fixed');
    const requestChallenge = this.challenge;
    const request = { schema_version: 1, session_id: this.sessionId, sequence: ++this.sequence, challenge: requestChallenge, op, ...extra };
    this.socket.write(`${JSON.stringify(request)}\n`);
    const reply = await this.frame(deadline);
    if (!reply || (reply.type !== 'reply' && reply.type !== 'error')) throw new Error('broker reply type is invalid');
    if (reply.type === 'error') throw new Error(`broker operation failed: ${reply.error?.code ?? 'operation-failed'}`);
    exact(reply, ['schema_version', 'type', 'session_id', 'sequence', 'challenge', 'result'], 'broker reply');
    if (reply.schema_version !== 1 || reply.session_id !== this.sessionId || reply.sequence !== this.sequence || reply.challenge === requestChallenge) throw new Error('broker reply identity is invalid');
    digest(reply.challenge, 'broker reply challenge');
    this.challenge = reply.challenge;
    if (!reply.result || typeof reply.result !== 'object') throw new Error('broker result is invalid');
    return validateRunningResult(reply.result, this.sessionId, this.sequence, requestChallenge);
  }

  async frame(deadline) {
    while (true) {
      const end = this.buffer.indexOf(10);
      if (end >= 0) {
        if (end > FRAME_BYTES) throw new Error('broker frame exceeds bound');
        const bytes = this.buffer.subarray(0, end);
        this.buffer = this.buffer.subarray(end + 1);
        return strictJson(bytes);
      }
      if (this.buffer.length > FRAME_BYTES) throw new Error('broker frame exceeds bound');
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new Error('broker frame timed out');
      const chunk = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => { cleanup(); reject(new Error('broker frame timed out')); }, remaining);
        const data = (value) => { cleanup(); resolve(value); };
        const error = (value) => { cleanup(); reject(value); };
        const end = () => { cleanup(); reject(new Error('broker closed before reply')); };
        const cleanup = () => { clearTimeout(timer); this.socket.off('data', data); this.socket.off('error', error); this.socket.off('end', end); };
        this.socket.once('data', data); this.socket.once('error', error); this.socket.once('end', end);
      });
      this.buffer = Buffer.concat([this.buffer, chunk]);
    }
  }

  close() {
    this.socket?.end();
    this.socket = null;
  }
}

async function runPlaywright(descriptorPath, scenario, viewer, endpoint, deadline) {
  const cli = path.join(projectRoot, 'node_modules/@playwright/test/cli.js');
  await ownerFile(cli, 'Playwright CLI', EVIDENCE_BYTES, false);
  const runtime = await boundViewerRuntime(viewer);
  const vehicleIds = scenario.vehicle_ids ?? scenario.vehicleIds;
  if (!Array.isArray(vehicleIds) || typeof vehicleIds[0] !== 'string') throw new Error('scenario does not contain a paging vehicle');
  const env = safeChildEnvironment({
    TESLATLAS_VIEWER_EXTERNAL_SERVER: '1',
    TESLATLAS_VIEWER_PAGE_ORIGIN: viewer.page.origin,
    TESLATLAS_VIEWER_BROWSER_EXECUTABLE: runtime.executablePath,
    TESLATLAS_VIEWER_BROWSER_LOG: viewer.reservations.browser_log,
    TESLATLAS_VIEWER_RAW_EVIDENCE_DIR: viewer.reservations.raw_evidence_dir,
    NODE_EXTRA_CA_CERTS: runtime.certificatePath,
    TESLATLAS_HUB_HTTP_CONFIG: descriptorPath,
    TESLATLAS_VIEWER_HUB_ENDPOINT: endpoint,
    TESLATLAS_VIEWER_HUB_TLS_IDENTITY: '',
    TESLATLAS_VIEWER_PAGING_VEHICLE_ID: vehicleIds[0],
    ...(typeof vehicleIds[1] === 'string' ? { TESLATLAS_VIEWER_EMPTY_VEHICLE_ID: vehicleIds[1] } : {}),
  });
  const child = spawn(process.execPath, [cli, 'test', 'e2e/installed-hub.spec.ts', '--config=playwright.hub.config.ts'], {
    cwd: projectRoot,
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: false,
  });
  let stdout = '';
  let stderr = '';
  const collect = (target, chunk) => { target.value = (target.value + String(chunk)).slice(-EVIDENCE_BYTES); };
  const out = { value: '' };
  const err = { value: '' };
  child.stdout.on('data', (chunk) => collect(out, chunk));
  child.stderr.on('data', (chunk) => collect(err, chunk));
  const remaining = Math.max(1, deadline - Date.now());
  const outcome = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { child.kill('SIGTERM'); reject(new Error('installed Viewer Playwright route timed out')); }, remaining);
    child.once('error', (error) => { clearTimeout(timer); reject(error); });
    child.once('exit', (code, signal) => { clearTimeout(timer); resolve({ code, signal }); });
  });
  stdout = out.value;
  stderr = err.value;
  return { ...outcome, stdout, stderr };
}

function pendingCases(cellId) {
  const serviceMode = cellId.endsWith('macos_arm64') ? 'installed-app-launchagent' : 'installed-deb-systemd';
  return CASES.map(([id, evidence_kind]) => ({
    id,
    status: 'pending',
    expected: id === 'installed_service_runtime' ? { service_mode: serviceMode } : { evidence: 'not-run' },
    actual: id === 'installed_service_runtime' ? { service_mode: serviceMode } : { evidence: 'not-run' },
    evidence_kind,
    request_transcript: [],
    ...(evidence_kind === 'identity' ? { process_evidence: { status: 'pending', reason: 'installed route is a smoke lane; full matrix journey is not admitted' } } : {}),
  }));
}

async function waitForAck(pathname, session, sessionHash, readyBinding, deadline) {
  while (Date.now() < deadline) {
    try {
      const { value: ack } = await privateJson(pathname, 'runner acknowledgement', 65_536);
      if (!ack || typeof ack !== 'object') throw new Error('runner acknowledgement is invalid');
      if (ack.schema_version !== 1 || ack.type !== 'ack' || ack.session_id !== session.session_id || ack.cell_id !== session.cell_id || ack.session_input_sha256 !== sessionHash || ack.sequence !== 1 || ack.phase !== 'evidence_ready' || ack.ready_sha256 !== readyBinding.sha256) throw new Error('runner acknowledgement identity is invalid');
      if (ack.status !== 'accepted' || ack.action !== 'close_completed') throw new Error('runner rejected Viewer evidence');
      return ack;
    } catch (error) {
      if (!String(error).includes('ENOENT') && !String(error).includes('no such file')) throw error;
      await new Promise((resolve) => setTimeout(resolve, Math.min(50, Math.max(1, deadline - Date.now()))));
    }
  }
  throw new Error('runner acknowledgement timed out');
}

async function main() {
  if (process.argv.includes('--help')) {
    console.log('Usage: node tools/matrix-live.mjs <private SessionInput JSON>');
    return;
  }
  if (process.argv.length !== 3) throw new Error('one private SessionInput path is required');
  const sessionPath = absolute(process.argv[2], 'session input path');
  const { value: session, bytes: sessionBytes } = await privateJson(sessionPath, 'session input');
  validateSession(session);
  const sessionHash = sha256(sessionBytes);
  const start = Date.now();
  const deadline = start + session.bounds.cell_timeout_ms;
  const outputs = session.outputs;
  await mkdir(outputs.coordination_dir, { recursive: true, mode: 0o700 });
  const coordinationMetadata = await stat(outputs.coordination_dir);
  if (!coordinationMetadata.isDirectory() || coordinationMetadata.uid !== process.getuid() || (coordinationMetadata.mode & 0o077) !== 0) throw new Error('coordination directory is not private');
  await mkdir(session.viewer.reservations.raw_evidence_dir, { recursive: false, mode: 0o700 });
  const rawEvidenceMetadata = await stat(session.viewer.reservations.raw_evidence_dir);
  if (!rawEvidenceMetadata.isDirectory() || rawEvidenceMetadata.uid !== process.getuid() || (rawEvidenceMetadata.mode & 0o077) !== 0) throw new Error('Viewer raw evidence directory is not private');
  const broker = new Broker(session.broker.socket_path, session.session_id);
  let finalObservation;
  let route;
  let descriptorPath;
  let invitationPath;
  try {
    const initial = await broker.open(deadline).then(() => broker.request('verify', {}, deadline));
    const paired = await broker.request('pair', {}, deadline);
    const descriptor = initial.descriptor ?? paired.descriptor;
    const invitation = paired.invitation ?? initial.invitation;
    if (!descriptor || typeof descriptor !== 'object' || !invitation || typeof invitation !== 'object') throw new Error('broker did not return a Viewer descriptor and invitation');
    const scenario = strictJson(await ownerFile(session.inputs.scenario.local.path, 'scenario input'));
    const { value: header } = await privateJson(session.header.local.path, 'matrix evidence header');
    exact(header, ['schema_version', 'execution_kind', 'adapter', 'cell_id', 'product_version', 'profile_id', 'profile_revision', 'profile_sha256', 'source_identities', 'artifacts', 'runtime'], 'matrix evidence header');
    if (header.schema_version !== 1 || header.execution_kind !== 'actual_hub_acceptance' || header.adapter !== 'viewer' || header.cell_id !== session.cell_id || header.product_version !== '2026.36.2' || header.profile_id !== 'hub-http-v1' || header.profile_revision !== '1.0.0' || header.profile_sha256 !== session.inputs.profile_manifest.local.sha256 || !Array.isArray(header.source_identities) || !Array.isArray(header.artifacts) || !header.runtime || typeof header.runtime !== 'object') throw new Error('matrix evidence header identity is invalid');
    const hubId = descriptor.hub_id ?? descriptor.hubId;
    const endpoint = descriptor.endpoint;
    const scenarioPath = session.inputs.scenario.local.path;
    const certificatePath = session.inputs.certificate.local.path;
    if (typeof hubId !== 'string' || typeof endpoint !== 'string') throw new Error('broker descriptor identity is incomplete');
    invitationPath = path.join(outputs.coordination_dir, 'viewer-invitation.json');
    descriptorPath = path.join(outputs.coordination_dir, 'viewer-http-config.json');
    await exclusiveJson(invitationPath, invitation, 'Viewer invitation');
    await exclusiveJson(descriptorPath, {
      schema_version: 1,
      status: 'ready',
      provenance: 'installed-package-service',
      endpoint,
      hub_id: hubId,
      profile_id: descriptor.profile_id ?? descriptor.profileId ?? 'hub-http-v1',
      profile_sha256: descriptor.profile_sha256 ?? descriptor.profileSha256 ?? null,
      scenario_path: scenarioPath,
      scenario_sha256: descriptor.scenario_sha256 ?? descriptor.scenarioSha256 ?? sha256(await ownerFile(scenarioPath, 'scenario input')),
      certificate_path: certificatePath,
      invitation_path: invitationPath,
    }, 'Viewer HTTP descriptor');
    route = await runPlaywright(descriptorPath, scenario, session.viewer, endpoint, deadline);
    if (route.code !== 0) throw new Error(`installed Viewer route failed (${route.code ?? route.signal})`);
    await exclusiveJson(session.viewer.reservations.browser_log, {
      schema_version: 1,
      browser: session.viewer.browser,
      page_origin: session.viewer.page.origin,
      hub_public_origin: session.viewer.hub.public_origin,
      exit_code: route.code,
      stdout: route.stdout,
      stderr: route.stderr,
    }, 'Viewer browser log');
    finalObservation = await broker.request('verify', {}, deadline);
    const normalized = {
      ...header,
      cases: pendingCases(session.cell_id),
    };
    const actorEvidence = {
      schema_version: 1,
      session_id: session.session_id,
      cell_id: session.cell_id,
      session_input_sha256: sessionHash,
      actors: session.actors.map((actor) => ({
        id: actor.id,
        kind: actor.kind,
        runtime_ref: actor.runtime_ref,
        entrypoint_ref: actor.entrypoint_ref,
        artifact_roles: actor.artifact_roles,
        source_roles: actor.source_roles,
        installed_manifest: actor.input_manifest.local,
        raw_evidence: [],
      })),
      invocations: [],
    };
    await exclusiveJson(outputs.normalized, normalized, 'normalized Viewer evidence');
    await exclusiveJson(outputs.actor_evidence, actorEvidence, 'Viewer actor evidence');
    const completion = await exclusiveJson(path.join(outputs.coordination_dir, 'adapter-completion.json'), {
      schema_version: 1,
      session_id: session.session_id,
      cell_id: session.cell_id,
      session_input_sha256: sessionHash,
      normalized: await binding(outputs.normalized, 'normalized Viewer evidence'),
      actor_evidence: await binding(outputs.actor_evidence, 'Viewer actor evidence'),
    }, 'Viewer adapter completion');
    const proof = finalObservation.proof;
    if (!proof || typeof proof !== 'object') throw new Error('final broker proof is missing');
    const proofSha = sha256(Buffer.from(JSON.stringify(canonicalize(proof)), 'utf8'));
    const ready = await exclusiveJson(path.join(outputs.coordination_dir, 'ready-000001.json'), {
      schema_version: 1,
      type: 'ready',
      session_id: session.session_id,
      cell_id: session.cell_id,
      session_input_sha256: sessionHash,
      instance_nonce: session.instance_nonce,
      sequence: 1,
      phase: 'evidence_ready',
      observation: { session_sequence: proof.sequence, proof_sha256: proofSha },
      evidence: completion,
    }, 'Viewer readiness');
    await waitForAck(path.join(outputs.coordination_dir, 'ack-000001.json'), session, sessionHash, ready, deadline);
    const frameworkLog = Buffer.from(`${route.stdout}\n${route.stderr}`, 'utf8');
    if (frameworkLog.length > EVIDENCE_BYTES) throw new Error('Viewer framework log exceeds bound');
    await exclusiveJson(outputs.framework_log, {
      schema_version: 1,
      command: [process.execPath, path.join(projectRoot, 'node_modules/@playwright/test/cli.js'), 'test', 'e2e/installed-hub.spec.ts', '--config=playwright.hub.config.ts'],
      exit_code: route.code,
      stdout: route.stdout,
      stderr: route.stderr,
    }, 'Viewer framework log');
  } finally {
    broker.close();
    // Do not attempt to revoke or stop the Hub here.  The runner owns the
    // independent close and stopped proof after the evidence barrier.
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(`installed Viewer matrix lane failed: ${error.message}`);
    process.exitCode = 1;
  });
}
