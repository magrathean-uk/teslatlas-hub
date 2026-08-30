import { FixtureDataSource } from './fixture-data-source';
import {
  ViewerDataError,
  type AppMode,
  type DiscoveredHub,
  type FixtureScenario,
  type HubSnapshot,
  type PairedDevice,
  type PairedHub,
  type PairingInput,
  type ViewerDataSource,
} from './types';

const sdkUnavailable = () =>
  new ViewerDataError(
    'SDK_NOT_RELEASED',
    'Live Hub access needs a released Teslatlas TypeScript SDK.',
  );

class UnavailableLiveDataSource implements ViewerDataSource {
  async discover(_signal?: AbortSignal): Promise<DiscoveredHub[]> {
    throw sdkUnavailable();
  }

  async pair(
    _input: PairingInput,
    _signal?: AbortSignal,
  ): Promise<PairedHub> {
    throw sdkUnavailable();
  }

  async readSnapshot(
    _scenario: FixtureScenario,
    _signal?: AbortSignal,
  ): Promise<HubSnapshot> {
    throw sdkUnavailable();
  }

  async removePairedDevice(
    _deviceId: string,
    _signal?: AbortSignal,
  ): Promise<PairedDevice[]> {
    throw sdkUnavailable();
  }
}

export function createDataSource(mode: AppMode): ViewerDataSource {
  return mode === 'fixture'
    ? new FixtureDataSource()
    : new UnavailableLiveDataSource();
}
