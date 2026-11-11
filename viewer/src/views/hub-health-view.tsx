import { Metric } from '../components/metric';
import type { ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { HubSnapshot } from '../data/types';
import { formatDateTime } from './view-helpers';

interface HubHealthViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
}

export function HubHealthView({
  snapshot,
  state,
}: HubHealthViewProps) {
  const labels: Record<ViewState, string> = {
    complete: 'Healthy',
    empty: 'No report',
    stale: 'Stale',
    inferred: 'Inferred',
    degraded: 'Degraded',
    offline: 'Offline',
  };

  return (
    <ViewFrame
      id="hub-health"
      eyebrow="Connection and capabilities"
      title="Hub health"
      description="Identity, protocol support, and the last confirmed Hub response."
      state={state}
      stateLabel={labels[state]}
    >
      {state === 'offline' && (
        <div className="notice-banner" data-tone="offline">
          <strong>Cannot reach this Hub.</strong>
          <span>Last known data remains visible and is not presented as live.</span>
        </div>
      )}
      {state === 'stale' && (
        <div className="notice-banner" data-tone="stale">
          <strong>Last known data</strong>
          <span>
            The Hub responded earlier, but its health check is now stale.
          </span>
        </div>
      )}
      {state === 'degraded' && (
        <div className="notice-banner" data-tone="degraded">
          <strong>Hub is responding with degraded collection.</strong>
          <span>Review data quality and collector freshness for details.</span>
        </div>
      )}
      {snapshot.resources.readiness.availability ===
        'temporarily-unavailable' && (
        <div
          className="notice-banner"
          data-tone={snapshot.resources.readiness.retained ? 'stale' : 'offline'}
          role="status"
        >
          <strong>
            {snapshot.resources.readiness.retained
              ? 'Showing retained readiness'
              : 'Readiness temporarily unavailable'}
          </strong>
          <span>
            {snapshot.resources.readiness.retained
              ? `Last confirmed readiness: ${snapshot.hub.readiness}${
                  snapshot.hub.readinessReason === null
                    ? ''
                    : ` — ${snapshot.hub.readinessReason}`
                }. ${snapshot.resources.readiness.detail ?? ''}`
              : snapshot.resources.readiness.detail}
          </span>
        </div>
      )}

      <div className="hub-hero panel">
        <div>
          <p className="card-kicker">Connected Hub</p>
          <h2>{snapshot.hub.displayName}</h2>
          <p className="mono-text">{snapshot.hub.endpoint}</p>
        </div>
        <div className="health-orbit" data-health={state} aria-hidden="true">
          <span />
        </div>
      </div>

      <dl className="metric-grid">
        <Metric label="Hub version" value={snapshot.hub.version} />
        <Metric label="Protocol" value={snapshot.hub.protocolVersion} />
        <Metric
          label="Last checked"
          value={formatDateTime(snapshot.hub.checkedAt)}
          detail="UTC"
        />
        <Metric
          label="Capabilities"
          value={snapshot.hub.capabilities.length}
          detail="advertised by this Hub"
        />
      </dl>

      <div className="panel identity-panel">
        <div>
          <p className="card-kicker">Pinned Hub identity</p>
          <p className="mono-text identity-value">{snapshot.hub.id}</p>
        </div>
        <dl className="identity-list">
          <div>
            <dt>TLS identity</dt>
            <dd>{snapshot.hub.tlsIdentity ?? 'Not supplied'}</dd>
          </div>
          <div>
            <dt>Manifest key</dt>
            <dd>{snapshot.hub.manifestKey ?? 'Not advertised'}</dd>
          </div>
          <div>
            <dt>Readiness</dt>
            <dd>{snapshot.hub.readiness}</dd>
          </div>
        </dl>
        <p>Endpoint changes must still match the pinned Hub UUID.</p>
      </div>

      <div className="panel">
        <h2>Advertised capabilities</h2>
        <ul className="tag-list" aria-label="Advertised capabilities">
          {snapshot.hub.capabilities.map((capability) => (
            <li key={capability}>{capability}</li>
          ))}
        </ul>
      </div>
    </ViewFrame>
  );
}
