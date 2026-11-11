import { useCallback, useEffect, useRef, useState } from 'react';

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
    drivePaging: Object.fromEntries(
      Object.entries(snapshot.drivePaging).map(([vehicleId, paging]) => [
        vehicleId,
        {
          ...paging,
          resource:
            paging.resource.availability === 'unsupported'
              ? paging.resource
              : {
                  availability: 'temporarily-unavailable' as const,
                  retained: paging.loadedCount > 0,
                  detail: error.message,
                },
        },
      ]),
    ),
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
  const generation = useRef(0);
  const activeOperation = useRef<{
    controller: AbortController;
    generation: number;
    kind: 'snapshot' | 'page';
  } | null>(null);
  const [loadingDriveVehicleId, setLoadingDriveVehicleId] = useState<string | null>(null);
  const [drivePageError, setDrivePageError] = useState<{
    vehicleId: string;
    message: string;
  } | null>(null);

  const beginOperation = useCallback((kind: 'snapshot' | 'page') => {
    if (activeOperation.current !== null) {
      return null;
    }
    const operation = {
      controller: new AbortController(),
      generation: generation.current + 1,
      kind,
    } as const;
    generation.current = operation.generation;
    activeOperation.current = operation;
    return operation;
  }, []);

  const isCurrentOperation = useCallback(
    (operation: NonNullable<typeof activeOperation.current>) =>
      activeOperation.current === operation &&
      generation.current === operation.generation &&
      !operation.controller.signal.aborted,
    [],
  );

  const finishOperation = useCallback(
    (operation: NonNullable<typeof activeOperation.current>) => {
      if (activeOperation.current !== operation) {
        return;
      }
      activeOperation.current = null;
      if (operation.kind === 'page') {
        setLoadingDriveVehicleId(null);
      }
    },
    [],
  );

  const invalidateOperation = useCallback(() => {
    const operation = activeOperation.current;
    if (operation !== null) {
      operation.controller.abort();
      activeOperation.current = null;
    }
    generation.current += 1;
  }, []);

  const startSnapshotRead = useCallback(
    (operation: NonNullable<typeof activeOperation.current>) => {
      setLoadingDriveVehicleId(null);
      setDrivePageError(null);
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

      void dataSource
        .readSnapshot(scenario, operation.controller.signal)
        .then((snapshot) => {
          if (!isCurrentOperation(operation)) {
            return;
          }
          setState({
            phase: 'ready',
            snapshot,
            error: null,
            refreshing: false,
          });
        })
        .catch((error: unknown) => {
          if (!isCurrentOperation(operation)) {
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
            setDrivePageError(null);
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
        })
        .finally(() => finishOperation(operation));
    },
    [dataSource, finishOperation, isCurrentOperation, scenario],
  );

  useEffect(() => {
    if (!enabled) {
      invalidateOperation();
      setState(idleState);
      setLoadingDriveVehicleId(null);
      setDrivePageError(null);
      return;
    }

    const operation = beginOperation('snapshot');
    if (operation !== null) {
      startSnapshotRead(operation);
    }

    return invalidateOperation;
  }, [beginOperation, dataSource, enabled, invalidateOperation, scenario, startSnapshotRead]);

  const reload = useCallback(() => {
    if (!enabled) return;
    const operation = beginOperation('snapshot');
    if (operation !== null) {
      startSnapshotRead(operation);
    }
  }, [beginOperation, enabled, startSnapshotRead]);

  const loadMoreDrives = useCallback(
    async (vehicleId: string) => {
      if (!enabled || dataSource.loadMoreDrives === undefined) return;
      const operation = beginOperation('page');
      if (operation === null) return;
      setLoadingDriveVehicleId(vehicleId);
      setDrivePageError(null);
      try {
        const snapshot = await dataSource.loadMoreDrives(
          vehicleId,
          operation.controller.signal,
        );
        if (isCurrentOperation(operation)) {
          setState({
            phase: 'ready',
            snapshot,
            error: null,
            refreshing: false,
          });
        }
      } catch (error: unknown) {
        if (!isCurrentOperation(operation)) return;
        const resolvedError =
          error instanceof Error ? error : new Error('Drive history unavailable.');
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
          setDrivePageError(null);
        } else {
          setDrivePageError({ vehicleId, message: resolvedError.message });
        }
      } finally {
        finishOperation(operation);
      }
    },
    [beginOperation, dataSource, enabled, finishOperation, isCurrentOperation],
  );

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

  return {
    ...state,
    reload,
    removeDevice,
    loadMoreDrives,
    loadingDriveVehicleId,
    drivePageError,
  };
}
