import type { CreateHubClientOptions, HubClient } from "../src/hub/models.js";

export interface BrowserPairingPreflightOptions {
  readonly createHubClient: (options: CreateHubClientOptions) => HubClient;
  readonly endpoint: string;
  readonly hubId: string;
  readonly credentials: CreateHubClientOptions["credentials"];
  readonly fetch?: typeof globalThis.fetch;
}

export interface BrowserPairingPreflightResult {
  readonly client: HubClient;
  readonly pairing: {
    readonly errorCode: "hub_tls_pin_unavailable";
    readonly networkRequestCount: 0;
  };
  readonly readsUseDefaultFetch: true;
}

export function createProvisionedBrowserClientAndCheckPairing(
  options: BrowserPairingPreflightOptions,
): Promise<BrowserPairingPreflightResult>;
