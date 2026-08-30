import { DataState } from '../components/data-state';
import { Metric } from '../components/metric';
import { StatusPill, type ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { HubSnapshot } from '../data/types';
import { formatDateTime, formatDuration, valueOrAbsent } from './view-helpers';

interface DataQualityViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
}

export function DataQualityView({ snapshot, state }: DataQualityViewProps) {
  const unresolved = snapshot.quality.gaps.filter((gap) => gap.status === 'open');

  return (
    <ViewFrame
      id="data-quality"
      eyebrow="Evidence, not guesswork"
      title="Data quality"
      description="Visible coverage and gaps show what the Hub knows and where it does not."
      state={state}
    >
      {snapshot.quality.overall === 'unknown' ? (
        <DataState
          title="No data-quality report"
          detail="The Hub returned no quality projection for this fixture state."
        />
      ) : (
        <>
          <dl className="metric-grid">
            <Metric
              label="Observed coverage"
              value={valueOrAbsent(
                snapshot.quality.observedCoveragePercent,
                '%',
              )}
            />
            <Metric
              label="Open issues"
              value={`${unresolved.length} unresolved ${
                unresolved.length === 1 ? 'gap' : 'gaps'
              }`}
            />
            <Metric
              label="Projection"
              value={snapshot.quality.overall}
              detail="viewer fixture classification"
            />
            <Metric
              label="Generated"
              value={formatDateTime(snapshot.quality.generatedAt)}
            />
          </dl>

          {unresolved.length === 0 ? (
            <DataState
              title="No unresolved gaps"
              detail="This fixture has no open telemetry gaps."
              kind="notice"
            />
          ) : (
            <div className="gap-list">
              {unresolved.map((gap) => {
                const vehicle = snapshot.vehicles.find(
                  (candidate) => candidate.id === gap.vehicleId,
                );
                return (
                  <article className="gap-card" key={gap.id}>
                    <header>
                      <div>
                        <p className="card-kicker">
                          {vehicle?.displayName ?? 'Unknown vehicle'}
                        </p>
                        <h2>{gap.reason}</h2>
                      </div>
                      <StatusPill state="degraded" label="Open gap" />
                    </header>
                    <dl className="compact-list">
                      <div>
                        <dt>Started</dt>
                        <dd>{formatDateTime(gap.startedAt)}</dd>
                      </div>
                      <div>
                        <dt>Duration</dt>
                        <dd>{formatDuration(gap.durationSeconds)}</dd>
                      </div>
                    </dl>
                  </article>
                );
              })}
            </div>
          )}
        </>
      )}
    </ViewFrame>
  );
}
