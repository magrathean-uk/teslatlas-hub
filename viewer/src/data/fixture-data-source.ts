import {
  createFixtureSnapshot,
  discoveredFixtureHub,
} from './fixture-data';
import {
  ViewerDataError,
  type DiscoveredHub,
  type FixtureScenario,
  type HubSnapshot,
  type PairedDevice,
  type PairedHub,
  type PairingInput,
  type ViewerDataSource,
} from './types';

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) {
    throw new DOMException('The operation was aborted.', 'AbortError');
  }
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

export class FixtureDataSource implements ViewerDataSource {
  async discover(signal?: AbortSignal): Promise<DiscoveredHub[]> {
    throwIfAborted(signal);
    return clone([discoveredFixtureHub]);
  }

  async pair(input: PairingInput, signal?: AbortSignal): Promise<PairedHub> {
    throwIfAborted(signal);

    if (
      input.hubId !== discoveredFixtureHub.id ||
      input.invitationCode !== '482731'
    ) {
      throw new ViewerDataError(
        'INVALID_INVITATION',
        'That fixture invitation is not valid.',
      );
    }

    if (!input.deviceName.trim()) {
      throw new ViewerDataError(
        'DEVICE_NAME_REQUIRED',
        'Enter a name for this viewer.',
      );
    }

    return {
      hubId: discoveredFixtureHub.id,
      deviceId: 'device-viewer',
      pairedAt: '2026-08-30T09:55:00.000Z',
      manifestKey: discoveredFixtureHub.manifestKey,
      tlsIdentity: discoveredFixtureHub.tlsIdentity,
    };
  }

  async readSnapshot(
    scenario: FixtureScenario,
    signal?: AbortSignal,
  ): Promise<HubSnapshot> {
    throwIfAborted(signal);

    if (scenario === 'error') {
      throw new ViewerDataError(
        'FIXTURE_FAILURE',
        'The fixture Hub could not be read.',
      );
    }

    if (scenario === 'loading') {
      return new Promise<HubSnapshot>((_resolve, reject) => {
        signal?.addEventListener(
          'abort',
          () => reject(new DOMException('The operation was aborted.', 'AbortError')),
          { once: true },
        );
      });
    }

    return createFixtureSnapshot(scenario);
  }

  async removePairedDevice(
    deviceId: string,
    signal?: AbortSignal,
  ): Promise<PairedDevice[]> {
    throwIfAborted(signal);
    const snapshot = createFixtureSnapshot('complete');
    snapshot.devices = snapshot.devices.filter((device) => device.id !== deviceId);
    return snapshot.devices;
  }

  async logout(): Promise<void> {
    // Fixture pairing exists only in the owning App state.
  }
}
