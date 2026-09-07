import { FixtureDataSource } from './fixture-data-source';
import { SdkDataSource } from './sdk-data-source';
import type { AppMode, ViewerDataSource } from './types';

export function createDataSource(mode: AppMode): ViewerDataSource {
  return mode === 'fixture'
    ? new FixtureDataSource()
    : new SdkDataSource();
}
