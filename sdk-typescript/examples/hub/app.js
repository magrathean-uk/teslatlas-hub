import { createHubClient } from "/sdk.js";

const endpointInput = document.querySelector("#endpoint");
const hubIdInput = document.querySelector("#hub-id");
const credentialInput = document.querySelector("#credential");
const status = document.querySelector("#status");
const connectButton = document.querySelector("#connect");
const refreshButton = document.querySelector("#refresh");
const disconnectButton = document.querySelector("#disconnect");
let client;
let activeCredentials;
let busy = false;
const requestTimeoutMs = 15_000;

connectButton.addEventListener("click", () => run(connect));
refreshButton.addEventListener("click", () => run(refresh));
disconnectButton.addEventListener("click", () => run(disconnect));

async function connect() {
  const endpoint = required(endpointInput.value, "endpoint");
  const hubId = required(hubIdInput.value, "Hub UUID");
  const credentialText = credentialInput.value.trim();
  const credential =
    credentialText.length > 0 ? readCredentialEnvelope(credentialText, endpoint, hubId) : undefined;
  credentialInput.value = "";
  const previousClient = client;
  previousClient?.dispose();
  client = undefined;
  activeCredentials = createCredentialStore(credential);
  const nextClient = createHubClient({
    endpoint,
    expectedHubId: hubId,
    credentials: activeCredentials,
  });
  client = nextClient;

  try {
    const discovery = await nextClient.discover(requestOptions());
    await nextClient.health(requestOptions());
    await nextClient.readiness(requestOptions());
    await readHub(nextClient, discovery.value);
  } catch (error) {
    nextClient.dispose();
    if (client === nextClient) {
      client = undefined;
      activeCredentials = undefined;
    }
    throw error;
  }
}

async function refresh() {
  if (client === undefined) throw new Error("connect first");
  const activeClient = client;
  const discovery = await activeClient.discover(requestOptions());
  await readHub(activeClient, discovery.value);
}

async function readHub(activeClient, discovery) {
  const vehicles = await activeClient.vehicles(requestOptions());
  const vehicleId = vehicles.value.vehicles[0]?.vehicleId;
  let current = "no vehicle";
  let drives = "unavailable";
  if (vehicleId !== undefined) {
    await activeClient.current(vehicleId, requestOptions());
    current = vehicleId;
    if (discovery.capabilities.includes("query.drives")) {
      const result = await activeClient.drives(vehicleId, {
        limit: 2,
        ...requestOptions(),
      });
      drives = result.kind;
    }
  }
  status.textContent = JSON.stringify(
    {
      connected: true,
      hubId: discovery.hubId,
      vehicleCount: vehicles.value.vehicles.length,
      current,
      drives,
    },
    null,
    2,
  );
}

async function disconnect() {
  const previousClient = client;
  try {
    if (previousClient !== undefined) await previousClient.logout();
  } finally {
    previousClient?.dispose();
    if (client === previousClient) {
      client = undefined;
      activeCredentials = undefined;
    }
    credentialInput.value = "";
    status.textContent = "Disconnected; provision a new credential through a pin-capable client.";
  }
}

async function run(operation) {
  if (busy) return;
  busy = true;
  connectButton.disabled = true;
  refreshButton.disabled = true;
  disconnectButton.disabled = true;
  status.textContent = "Working…";
  try {
    await operation();
  } catch (error) {
    if (isUnauthorized(error)) {
      await expireAuthentication();
      status.textContent =
        "Authentication expired; provision a new credential through a pin-capable client.";
    } else {
      status.textContent = formatError(error);
    }
  } finally {
    busy = false;
    connectButton.disabled = false;
    refreshButton.disabled = client === undefined;
    disconnectButton.disabled = client === undefined;
  }
}

function createCredentialStore(initialCredential) {
  let credential = initialCredential;
  return {
    load: () => credential,
    save: (value) => {
      credential = value;
    },
    clear: () => {
      credential = undefined;
    },
  };
}

function readCredentialEnvelope(value, expectedEndpoint, expectedHubId) {
  let envelope;
  try {
    envelope = JSON.parse(value);
  } catch {
    throw { code: "invalid_credential_json" };
  }
  if (
    envelope === null ||
    typeof envelope !== "object" ||
    envelope.endpoint !== expectedEndpoint ||
    envelope.hubId !== expectedHubId ||
    envelope.credential === null ||
    typeof envelope.credential !== "object"
  ) {
    throw { code: "credential_binding_mismatch" };
  }
  return envelope.credential;
}

function requestOptions() {
  return { signal: AbortSignal.timeout(requestTimeoutMs) };
}

async function expireAuthentication() {
  const previousClient = client;
  client = undefined;
  activeCredentials = undefined;
  await previousClient?.logout().catch(() => undefined);
  previousClient?.dispose();
  credentialInput.value = "";
}

function isUnauthorized(error) {
  return error !== null && typeof error === "object" && error.status === 401;
}

function required(value, name) {
  if (value.trim().length === 0) throw new Error(`${name} is required`);
  return value.trim();
}

function formatError(error) {
  if (!error || typeof error !== "object") return "Hub request failed";
  const code = typeof error.code === "string" ? error.code : "unknown_error";
  const statusCode = Number.isInteger(error.status) ? ` (HTTP ${error.status})` : "";
  return `${code}${statusCode}`;
}
