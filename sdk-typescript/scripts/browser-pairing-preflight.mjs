export async function createProvisionedBrowserClientAndCheckPairing({
  createHubClient,
  endpoint,
  hubId,
  credentials,
  fetch = globalThis.fetch,
}) {
  let requestCount = 0;
  const observedFetch = async (...arguments_) => {
    requestCount += 1;
    return fetch(...arguments_);
  };
  const preflightClient = createHubClient({
    endpoint,
    expectedHubId: hubId,
    credentials,
    fetch: observedFetch,
  });
  const pairingId = hubId;
  const secret = "0".repeat(64);
  const tlsPin = "1".repeat(64);
  const invitation = {
    endpoint,
    expiresAtMs: Date.now() + 60_000,
    pairingId,
    pairingUri: `teslatlas-hub://pair?endpoint=${encodeURIComponent(endpoint)}&pairing_id=${pairingId}&secret=${secret}&tls_pin=${tlsPin}`,
    secret,
    tlsPin,
  };

  let error;
  try {
    await preflightClient.claimPairing(invitation, "Browser preflight");
  } catch (caught) {
    error = caught;
  }
  if (error?.code !== "hub_tls_pin_unavailable") {
    preflightClient.dispose();
    throw new Error("browser invitation pairing did not fail with hub_tls_pin_unavailable");
  }
  if (requestCount !== 0) {
    preflightClient.dispose();
    throw new Error("browser invitation pairing dispatched network I/O");
  }
  preflightClient.dispose();

  const client = createHubClient({ endpoint, expectedHubId: hubId, credentials });

  return {
    client,
    pairing: {
      errorCode: error.code,
      networkRequestCount: requestCount,
    },
    readsUseDefaultFetch: true,
  };
}
