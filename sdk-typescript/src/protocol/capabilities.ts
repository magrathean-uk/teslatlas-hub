import { asCapabilityId } from "../core/capabilities.js";
import { MissingCapabilityError } from "../core/errors.js";
import type { HubDescriptor } from "./models.js";

export type Capability = HubDescriptor["capabilities"][number];

export function requireCapability(descriptor: HubDescriptor, capability: string): Capability {
  const capabilityId = asCapabilityId(capability);
  const found = descriptor.capabilities.find((candidate) => candidate.id === capabilityId);
  if (found === undefined) {
    throw new MissingCapabilityError(capabilityId);
  }
  return found;
}
