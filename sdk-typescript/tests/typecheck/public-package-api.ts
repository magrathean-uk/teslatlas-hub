import type { HubDescriptor, TeslatlasClient } from "@teslatlas/sdk";

declare const client: TeslatlasClient;

const descriptor: HubDescriptor = client.descriptor;
void descriptor;
