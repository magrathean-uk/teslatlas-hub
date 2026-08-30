import { DataState } from '../components/data-state';
import { StatusPill, type ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { CollectorSummary, HubSnapshot } from '../data/types';
import { formatDateTime, formatDuration } from './view-helpers';

interface CollectorFreshnessViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
}

function collectorState(collector: CollectorSummary): ViewState {
  if (collector.status === 'offline') return 'offline';
  if (collector.status === 'degraded') return 'degraded';
  return 'complete';
}

export function CollectorFreshnessView({
  snapshot,
  state,
}: CollectorFreshnessViewProps) {
  return (
    <ViewFrame
      id="collector-freshness"
      eyebrow="Independent collection paths"
      title="Collector freshness"
      description="Last event age and health are shown separately for each public source."
      state={state}
    >
      {snapshot.collectors.length === 0 ? (
        <DataState
          title="No collectors reported"
          detail="The Hub returned no collector-health entries."
        />
      ) : (
        <div className="card-grid">
          {snapshot.collectors.map((collector) => (
            <article className="collector-card" key={collector.id}>
              <header>
                <div>
                  <p className="card-kicker">{collector.source}</p>
                  <h2>{collector.displayName}</h2>
                </div>
                <StatusPill
                  state={collectorState(collector)}
                  label={collector.status}
                />
              </header>
              <p className="collector-age">
                {formatDuration(collector.lagSeconds)}
              </p>
              <p className="collector-label">since the latest event</p>
              <p>{collector.detail}</p>
              <p className="evidence-line">
                Last event: {formatDateTime(collector.lastEventAt)}
              </p>
            </article>
          ))}
        </div>
      )}
    </ViewFrame>
  );
}
