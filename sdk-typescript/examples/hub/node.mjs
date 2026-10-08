import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { createHubClient } from "@teslatlas/sdk/node";

class ConsumerInputError extends Error {
  constructor(message) {
    super(message);
    this.name = "ConsumerInputError";
  }
}

async function main() {
  let client;
  try {
    const args = parseArgs(process.argv.slice(2));
    if (args.help) {
      printUsage();
      return;
    }

    const endpoint = required(args.endpoint, "--endpoint");
    const hubId = required(args.hubId, "--hub-id");
    const hasInvitation = args.invitationFile !== undefined;
    const hasCredential = args.credentialFile !== undefined;
    if (hasInvitation === hasCredential) {
      throw new ConsumerInputError(
        "exactly one of --invitation-file or --credential-file is required",
      );
    }
    const timeoutMs = parseTimeout(args.timeoutMs);
    const invitation = hasInvitation
      ? await readJsonFile(resolve(args.invitationFile), "invitation file")
      : undefined;
    const credential = hasCredential
      ? await readCredentialEnvelope(resolve(args.credentialFile), endpoint, hubId)
      : undefined;
    let currentCredential = credential;
    const credentials = {
      load: () => currentCredential,
      save: (next) => {
        currentCredential = next;
      },
      clear: () => {
        currentCredential = undefined;
      },
    };
    client = createHubClient({
      endpoint,
      expectedHubId: hubId,
      credentials,
    });
    const requestOptions = () => ({ signal: AbortSignal.timeout(timeoutMs) });

    const discovery = await client.discover(requestOptions());
    await client.health(requestOptions());
    await client.readiness(requestOptions());
    if (invitation !== undefined) {
      await client.claimPairing(
        invitation,
        args.label ?? "Teslatlas Node example",
        requestOptions(),
      );
    }

    const vehicles = await client.vehicles(requestOptions());
    const vehicleId = vehicles.value.vehicles[0]?.vehicleId;
    let driveSummary = "drives unavailable";
    if (vehicleId !== undefined) {
      await client.current(vehicleId, requestOptions());
      if (discovery.value.capabilities.includes("query.drives")) {
        const drives = await client.drives(vehicleId, { limit: 2, ...requestOptions() });
        driveSummary = drives.kind === "page" ? "1 drive page(s)" : "drives not modified";
      }
    }
    if (args.credentialOut !== undefined && currentCredential !== undefined) {
      await writeCredentialEnvelope(
        resolve(args.credentialOut),
        endpoint,
        hubId,
        currentCredential,
      );
    }
    console.log(
      `Teslatlas Hub Node consumer: ${vehicles.value.vehicles.length} vehicle(s), ${driveSummary}, Hub ${discovery.value.hubId}`,
    );
  } catch (error) {
    if (client !== undefined && isUnauthorized(error)) {
      await client.logout().catch(() => undefined);
      throw new ConsumerInputError("authentication expired; pair again with a new invitation");
    }
    throw error;
  } finally {
    client?.dispose();
  }
}

function parseArgs(values) {
  const result = {};
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (value === "--help") result.help = true;
    else if (value === "--endpoint") result.endpoint = nextValue(values, ++index, value);
    else if (value === "--hub-id") result.hubId = nextValue(values, ++index, value);
    else if (value === "--invitation-file")
      result.invitationFile = nextValue(values, ++index, value);
    else if (value === "--credential-file")
      result.credentialFile = nextValue(values, ++index, value);
    else if (value === "--credential-out") result.credentialOut = nextValue(values, ++index, value);
    else if (value === "--label") result.label = nextValue(values, ++index, value);
    else if (value === "--timeout-ms") result.timeoutMs = nextValue(values, ++index, value);
    else throw new ConsumerInputError(`unknown option ${value}`);
  }
  return result;
}

function nextValue(values, index, option) {
  const value = values[index];
  if (value === undefined || value.startsWith("--")) {
    throw new ConsumerInputError(`${option} requires a value`);
  }
  return value;
}

async function readJsonFile(path, label) {
  let source;
  try {
    source = await readFile(path, "utf8");
  } catch {
    throw new ConsumerInputError(`${label} cannot be read`);
  }
  try {
    return JSON.parse(source);
  } catch {
    throw new ConsumerInputError(`${label} JSON is invalid`);
  }
}

async function readCredentialEnvelope(path, expectedEndpoint, expectedHubId) {
  const envelope = await readJsonFile(path, "credential file");
  if (
    envelope === null ||
    typeof envelope !== "object" ||
    envelope.endpoint !== expectedEndpoint ||
    envelope.hubId !== expectedHubId
  ) {
    throw new ConsumerInputError("credential file is bound to another endpoint or Hub");
  }
  return envelope.credential;
}

async function writeCredentialEnvelope(path, expectedEndpoint, expectedHubId, value) {
  const document = JSON.stringify(
    { endpoint: expectedEndpoint, hubId: expectedHubId, credential: value },
    null,
    2,
  );
  try {
    await writeFile(path, `${document}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
  } catch {
    throw new ConsumerInputError("credential output cannot be created exclusively");
  }
}

function required(value, option) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new ConsumerInputError(`${option} is required`);
  }
  return value.trim();
}

function parseTimeout(value) {
  const timeoutMs = value === undefined ? 15_000 : Number(value);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) {
    throw new ConsumerInputError("--timeout-ms must be a positive integer");
  }
  return timeoutMs;
}

function isUnauthorized(error) {
  return error !== null && typeof error === "object" && error.status === 401;
}

function formatError(error) {
  if (error instanceof ConsumerInputError) return error.message;
  if (!error || typeof error !== "object") return "Hub request failed";
  const code = typeof error.code === "string" ? error.code : "unknown_error";
  const status = Number.isInteger(error.status) ? ` (HTTP ${error.status})` : "";
  return `${code}${status}`;
}

function printUsage() {
  console.log(
    `Usage: node node.mjs --endpoint URL --hub-id UUID [--invitation-file PATH | --credential-file PATH] [--credential-out PATH] [--label TEXT] [--timeout-ms N]`,
  );
}

main().catch((error) => {
  console.error(`Teslatlas Hub Node consumer: ${formatError(error)}`);
  process.exitCode = 1;
});
