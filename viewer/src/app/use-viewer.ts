import { useCallback, useEffect, useState } from 'react';

import type {
  FixtureScenario,
  HubSnapshot,
  ViewerDataSource,
} from '../data/types';
import { ViewerDataError } from '../data/types';

type ViewerState =
  | {
      phase: 'idle';
      snapshot: null;
      error: null;
      refreshing: false;
    }
  | {
      phase: 'loading';
      snapshot: null;
      error: null;
      refreshing: true;
    }
  | {
      phase: 'ready';
      snapshot: HubSnapshot;
      error: Error | null;
      refreshing: boolean;
    }
  | {
      phase: 'error';
      snapshot: null;
      error: Error;
      refreshing: false;
    };

const idleState: ViewerState = {
  phase: 'idle',
  snapshot: null,
  error: null,
  refreshing: false,
};

function retainedSnapshot(snapshot: HubSnapshot, error: Error): HubSnapshot {
  const resources = Object.fromEntries(
    Object.entries(snapshot.resources).map(([name, resource]) => [
      name,
      resource.availability === 'unsupported'
        ? resource
        : {
            availability: 'temporarily-unavailable' as const,
            retained: true,
            detail: error.message,
          },
    ]),
  ) as HubSnapshot['resources'];
  return {
    ...snapshot,
    hub: { ...snapshot.hub, status: 'offline', freshness: 'stale' },
    vehicles: snapshot.vehicles.map((vehicle) => ({
      ...vehicle,
      freshness: 'stale',
    })),
    currentByVehicle: Object.fromEntries(
      Object.entries(snapshot.currentByVehicle).map(([vehicleId, current]) => [
        vehicleId,
        current === null ? null : { ...current, freshness: 'stale' },
      ]),
    ),
    resources,
  };
}

export function useViewer(
  dataSource: ViewerDataSource,
  scenario: FixtureScenario,
  enabled: boolean,
) {
  const [state, setState] = useState<ViewerState>(
    enabled
      ? {
          phase: 'loading',
          snapshot: null,
          error: null,
          refreshing: true,
        }
      : idleState,
  );
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    if (!enabled) {
      setState(idleState);
      return;
    }

    const controller = new AbortController();
    setState((current) =>
      current.snapshot === null
        ? {
            phase: 'loading',
            snapshot: null,
            error: null,
            refreshing: true,
          }
        : {
            phase: 'ready',
            snapshot: current.snapshot,
            error: null,
            refreshing: true,
          },
    );

    dataSource
      .readSnapshot(scenario, controller.signal)
      .then((snapshot) => {
        if (!controller.signal.aborted) {
          setState({
            phase: 'ready',
            snapshot,
            error: null,
            refreshing: false,
          });
        }
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        const resolvedError =
          error instanceof Error ? error : new Error('Unknown viewer error');
        if (
          resolvedError instanceof ViewerDataError &&
          resolvedError.code === 'AUTH_LOST'
        ) {
          setState({
            phase: 'error',
            snapshot: null,
            error: resolvedError,
            refreshing: false,
          });
          return;
        }
        setState((current) =>
          current.snapshot === null
            ? {
                phase: 'error',
                snapshot: null,
                error: resolvedError,
                refreshing: false,
              }
            : {
                phase: 'ready',
                snapshot: retainedSnapshot(current.snapshot, resolvedError),
                error: resolvedError,
                refreshing: false,
              },
        );
      });

    return () => controller.abort();
  }, [dataSource, enabled, reloadKey, scenario]);

  const reload = useCallback(() => setReloadKey((key) => key + 1), []);

  const removeDevice = useCallback(
    async (deviceId: string) => {
      const devices = await dataSource.removePairedDevice(deviceId);
      setState((current) =>
        current.phase === 'ready'
          ? {
              phase: 'ready',
              snapshot: { ...current.snapshot, devices },
              error: current.error,
              refreshing: current.refreshing,
            }
          : current,
      );
    },
    [dataSource],
  );

  return { ...state, reload, removeDevice };
}
