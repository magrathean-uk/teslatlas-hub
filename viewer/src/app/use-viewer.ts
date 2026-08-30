import { useCallback, useEffect, useState } from 'react';

import type {
  FixtureScenario,
  HubSnapshot,
  ViewerDataSource,
} from '../data/types';

type ViewerState =
  | { phase: 'idle'; snapshot: null; error: null }
  | { phase: 'loading'; snapshot: null; error: null }
  | { phase: 'ready'; snapshot: HubSnapshot; error: null }
  | { phase: 'error'; snapshot: null; error: Error };

const idleState: ViewerState = {
  phase: 'idle',
  snapshot: null,
  error: null,
};

export function useViewer(
  dataSource: ViewerDataSource,
  scenario: FixtureScenario,
  enabled: boolean,
) {
  const [state, setState] = useState<ViewerState>(
    enabled
      ? { phase: 'loading', snapshot: null, error: null }
      : idleState,
  );
  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    if (!enabled) {
      setState(idleState);
      return;
    }

    const controller = new AbortController();
    setState({ phase: 'loading', snapshot: null, error: null });

    dataSource
      .readSnapshot(scenario, controller.signal)
      .then((snapshot) => {
        if (!controller.signal.aborted) {
          setState({ phase: 'ready', snapshot, error: null });
        }
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        setState({
          phase: 'error',
          snapshot: null,
          error: error instanceof Error ? error : new Error('Unknown viewer error'),
        });
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
              error: null,
            }
          : current,
      );
    },
    [dataSource],
  );

  return { ...state, reload, removeDevice };
}
