import { useEffect, useState } from 'react';

import { ConnectionPanel } from '../components/connection-panel';
import type { ViewState } from '../components/status-pill';
import type {
  AppMode,
  FixtureScenario,
  HubSnapshot,
  PairedHub,
  ViewerDataSource,
} from '../data/types';
import { ViewerDataError } from '../data/types';
import { CollectorFreshnessView } from '../views/collector-freshness-view';
import { CurrentStateView } from '../views/current-state-view';
import { DataQualityView } from '../views/data-quality-view';
import { HubHealthView } from '../views/hub-health-view';
import { PairedDevicesView } from '../views/paired-devices-view';
import { RecentSessionsView } from '../views/recent-sessions-view';
import { VehiclesView } from '../views/vehicles-view';
import { useViewer } from './use-viewer';

interface AppProps {
  dataSource: ViewerDataSource;
  mode?: AppMode;
  initialScenario?: FixtureScenario;
  initialPaired?: boolean;
}

type ViewId =
  | 'health'
  | 'vehicles'
  | 'current'
  | 'sessions'
  | 'quality'
  | 'freshness'
  | 'devices';

const views: Array<{ id: ViewId; label: string; mark: string }> = [
  { id: 'health', label: 'Hub health', mark: '01' },
  { id: 'vehicles', label: 'Vehicles', mark: '02' },
  { id: 'current', label: 'Current state', mark: '03' },
  { id: 'sessions', label: 'Recent sessions', mark: '04' },
  { id: 'quality', label: 'Data quality', mark: '05' },
  { id: 'freshness', label: 'Collector freshness', mark: '06' },
  { id: 'devices', label: 'Paired devices', mark: '07' },
];

const scenarios: FixtureScenario[] = [
  'complete',
  'empty',
  'stale',
  'inferred',
  'degraded',
  'offline',
  'error',
  'loading',
];

function stateForView(view: ViewId, snapshot: HubSnapshot): ViewState {
  const isOffline = snapshot.hub.status === 'offline';
  const isStale = snapshot.hub.freshness === 'stale';
  const resourceState = (
    resource: keyof HubSnapshot['resources'],
  ): ViewState | null => {
    const state = snapshot.resources[resource];
    if (state.availability !== 'temporarily-unavailable') return null;
    return state.retained ? 'stale' : 'offline';
  };

  if (view === 'health') {
    if (
      snapshot.resources.health.availability === 'temporarily-unavailable'
    ) {
      return 'offline';
    }
    if (isOffline) return 'offline';
    if (snapshot.hub.status === 'degraded') return 'degraded';
    if (isStale) return 'stale';
    return 'complete';
  }

  if (view === 'vehicles') {
    const vehiclesState = resourceState('vehicles');
    if (vehiclesState !== null) return vehiclesState;
    if (snapshot.vehicles.length === 0) return 'empty';
    if (isOffline) return 'offline';
    if (snapshot.vehicles.some((vehicle) => vehicle.freshness === 'stale')) {
      return 'stale';
    }
    return 'complete';
  }

  if (view === 'current') {
    const currentState = resourceState('current');
    if (currentState !== null) return currentState;
    const current = Object.values(snapshot.currentByVehicle).filter(Boolean);
    if (current.length === 0) return 'empty';
    if (isOffline) return 'offline';
    if (current.some((value) => value?.freshness === 'stale')) return 'stale';
    if (current.some((value) => value && value.inferredFields.length > 0)) {
      return 'inferred';
    }
    return 'complete';
  }

  if (view === 'sessions') {
    const drivesState = resourceState('drives');
    if (drivesState !== null) return drivesState;
    const sessions = [...snapshot.drives, ...snapshot.charges];
    if (sessions.length === 0) return 'empty';
    if (isOffline) return 'offline';
    if (sessions.some((session) => session.quality?.level === 'degraded')) {
      return 'degraded';
    }
    if (
      sessions.some((session) => (session.quality?.derivedFields.length ?? 0) > 0)
    ) {
      return 'inferred';
    }
    if (isStale) return 'stale';
    return 'complete';
  }

  if (view === 'quality') {
    if (isOffline) return 'offline';
    if (snapshot.quality === null) return 'empty';
    if (snapshot.quality.overall === 'unknown') return 'empty';
    if (snapshot.quality.overall === 'degraded') return 'degraded';
    if (isStale) return 'stale';
    return 'complete';
  }

  if (view === 'freshness') {
    if (snapshot.collectors.length === 0) return 'empty';
    if (snapshot.collectors.every((collector) => collector.status === 'offline')) {
      return 'offline';
    }
    if (
      snapshot.collectors.some(
        (collector) =>
          collector.status === 'offline' || collector.status === 'degraded',
      )
    ) {
      return isStale ? 'stale' : 'degraded';
    }
    return 'complete';
  }

  if (isOffline) return 'offline';
  return snapshot.devices.length === 0 ? 'empty' : 'complete';
}

export function App({
  dataSource,
  mode = 'fixture',
  initialScenario = 'complete',
  initialPaired = true,
}: AppProps) {
  const [isPaired, setIsPaired] = useState(
    mode === 'fixture' && initialPaired,
  );
  const [scenario, setScenario] = useState(initialScenario);
  const [activeView, setActiveView] = useState<ViewId>('health');
  const [connectionNotice, setConnectionNotice] = useState<string | null>(null);
  const viewer = useViewer(dataSource, scenario, isPaired);

  useEffect(() => {
    if (
      mode === 'live' &&
      viewer.phase === 'error' &&
      viewer.error instanceof ViewerDataError &&
      viewer.error.code === 'AUTH_LOST'
    ) {
      setIsPaired(false);
      setConnectionNotice('Access ended; pair again to continue.');
      void dataSource.logout();
    }
  }, [dataSource, mode, viewer.error, viewer.phase]);

  function completePairing(_pairing: PairedHub) {
    setConnectionNotice(null);
    setIsPaired(true);
  }

  function loadCompleteFixture() {
    setScenario('complete');
  }

  async function clearLocalSession() {
    setActiveView('health');
    setScenario('complete');
    setConnectionNotice(null);
    setIsPaired(false);
    await dataSource.logout();
  }

  return (
    <div className="app-root">
      <a href="#main-content" className="skip-link">
        Skip to content
      </a>
      <header className="site-header">
        <a href="/" className="wordmark" aria-label="Teslatlas viewer home">
          <span aria-hidden="true" className="wordmark-mark">
            T
          </span>
          <span>
            Teslatlas <strong>viewer</strong>
          </span>
        </a>
        <div className="header-actions">
          <a
            className="header-action"
            href={
              mode === 'fixture'
                ? '/?mode=live&paired=false'
                : '/?mode=fixture'
            }
          >
            {mode === 'fixture' ? 'Connect to my Hub' : 'View demo'}
          </a>
          <span className="mode-chip">
            {mode === 'fixture' ? 'Fixture mode' : 'Live mode'}
          </span>
        </div>
      </header>

      {!isPaired && (
        <ConnectionPanel
          dataSource={dataSource}
          mode={mode}
          onPaired={completePairing}
          notice={connectionNotice}
        />
      )}

      {isPaired && viewer.phase === 'loading' && (
        <main
          id="main-content"
          className="loading-layout"
          data-view-state="loading"
          aria-busy="true"
        >
          <div
            role="status"
            aria-label="Loading Hub data"
            className="state-card state-card--stacked"
          >
            <span className="state-card-message">
              <span className="spinner" aria-hidden="true" />
              <span>Loading Hub data…</span>
            </span>
            <button type="button" onClick={clearLocalSession}>
              Change connection
            </button>
            {mode === 'fixture' && scenario === 'loading' && (
              <button type="button" onClick={loadCompleteFixture}>
                Load complete fixture
              </button>
            )}
          </div>
        </main>
      )}

      {isPaired && viewer.phase === 'error' && (
        <main
          id="main-content"
          className="loading-layout"
          data-view-state="error"
        >
          <div role="alert" className="state-card state-card--error">
            <strong>Hub data unavailable</strong>
            <span>{viewer.error.message}</span>
            <button type="button" onClick={viewer.reload}>
              Try again
            </button>
            <button type="button" onClick={clearLocalSession}>
              Change connection
            </button>
            {mode === 'fixture' && scenario === 'error' && (
              <button type="button" onClick={loadCompleteFixture}>
                Load complete fixture
              </button>
            )}
          </div>
        </main>
      )}

      {isPaired && viewer.phase === 'ready' && (
        <main id="main-content" className="viewer-layout">
          <aside className="sidebar" aria-label="Hub navigation">
            <div className="sidebar-hub">
              <span
                className="hub-avatar"
                data-health={viewer.snapshot.hub.status}
                aria-hidden="true"
              >
                H
              </span>
              <div>
                <p className="card-kicker">Connected to</p>
                <p className="sidebar-hub-name">
                  {viewer.snapshot.hub.displayName}
                </p>
              </div>
            </div>

            <nav aria-label="Viewer sections" className="section-navigation">
              <ul>
                {views.map((view) => (
                  <li key={view.id}>
                    <button
                      type="button"
                      onClick={() => setActiveView(view.id)}
                      aria-current={activeView === view.id ? 'page' : undefined}
                    >
                      <span aria-hidden="true">{view.mark}</span>
                      {view.label}
                    </button>
                  </li>
                ))}
              </ul>
            </nav>

            {mode === 'fixture' && (
              <div className="fixture-control">
                <label htmlFor="fixture-state">Fixture state</label>
                <select
                  id="fixture-state"
                  value={scenario}
                  onChange={(event) =>
                    setScenario(event.target.value as FixtureScenario)
                  }
                >
                  {scenarios.map((value) => (
                    <option value={value} key={value}>
                      {value.charAt(0).toUpperCase() + value.slice(1)}
                    </option>
                  ))}
                </select>
                <p>Deterministic, local, and redacted.</p>
              </div>
            )}

            <div className="session-control">
              <button
                type="button"
                className="secondary-button"
                onClick={viewer.reload}
                disabled={viewer.refreshing || viewer.loadingDriveVehicleId !== null}
              >
                {viewer.refreshing ? 'Refreshing Hub data…' : 'Refresh Hub data'}
              </button>
              <button
                type="button"
                className="secondary-button"
                onClick={clearLocalSession}
              >
                Clear local session
              </button>
              <p>Clears viewer memory only. It does not revoke a Hub device.</p>
            </div>
          </aside>

          <div className="viewer-content">
            {(viewer.refreshing || viewer.error) && (
              <div
                className="notice-banner"
                data-tone={viewer.error ? 'stale' : 'neutral'}
                role="status"
              >
                <strong>
                  {viewer.error ? 'Showing retained data' : 'Refreshing Hub data'}
                </strong>
                <span>
                  {viewer.error?.message ??
                    'The latest successful values remain visible while reads finish.'}
                </span>
              </div>
            )}
            {activeView === 'health' && (
              <HubHealthView
                snapshot={viewer.snapshot}
                state={stateForView('health', viewer.snapshot)}
              />
            )}
            {activeView === 'vehicles' && (
              <VehiclesView
                snapshot={viewer.snapshot}
                state={stateForView('vehicles', viewer.snapshot)}
              />
            )}
            {activeView === 'current' && (
              <CurrentStateView
                snapshot={viewer.snapshot}
                state={stateForView('current', viewer.snapshot)}
              />
            )}
            {activeView === 'sessions' && (
              <RecentSessionsView
                snapshot={viewer.snapshot}
                state={stateForView('sessions', viewer.snapshot)}
                onLoadMore={viewer.loadMoreDrives}
                loadingVehicleId={viewer.loadingDriveVehicleId}
                pageError={viewer.drivePageError}
                pagingDisabled={
                  viewer.refreshing || viewer.loadingDriveVehicleId !== null
                }
              />
            )}
            {activeView === 'quality' && (
              <DataQualityView
                snapshot={viewer.snapshot}
                state={stateForView('quality', viewer.snapshot)}
              />
            )}
            {activeView === 'freshness' && (
              <CollectorFreshnessView
                snapshot={viewer.snapshot}
                state={stateForView('freshness', viewer.snapshot)}
              />
            )}
            {activeView === 'devices' && (
              <PairedDevicesView
                snapshot={viewer.snapshot}
                state={stateForView('devices', viewer.snapshot)}
                onRemove={viewer.removeDevice}
              />
            )}
          </div>
        </main>
      )}
    </div>
  );
}
