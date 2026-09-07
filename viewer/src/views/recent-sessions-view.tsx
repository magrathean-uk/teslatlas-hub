import { DataState } from '../components/data-state';
import type { ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { HubSnapshot } from '../data/types';
import {
  formatDateTime,
  QualityPill,
  valueOrAbsent,
} from './view-helpers';

interface RecentSessionsViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
}

export function RecentSessionsView({
  snapshot,
  state,
}: RecentSessionsViewProps) {
  const drivesResource = snapshot.resources.drives;
  const vehicleName = (vehicleId: string) =>
    snapshot.vehicles.find((vehicle) => vehicle.id === vehicleId)?.displayName ??
    'Unknown vehicle';

  return (
    <ViewFrame
      id="recent-sessions"
      eyebrow="Bounded history"
      title="Recent sessions"
      description="Latest drive and charging summaries with quality evidence alongside values."
      state={state}
    >
      <section className="session-section" aria-labelledby="recent-drives-title">
        <div className="section-heading">
          <div>
            <p className="card-kicker">Movement</p>
            <h2 id="recent-drives-title">Recent drives</h2>
          </div>
          <span className="count-chip">{snapshot.drives.length}</span>
        </div>
        {drivesResource.availability === 'temporarily-unavailable' &&
          (drivesResource.retained ? (
            <div className="notice-banner" data-tone="stale" role="status">
              <strong>Showing retained drive sessions</strong>
              <span>{drivesResource.detail}</span>
            </div>
          ) : (
            <DataState
              title={
                snapshot.drives.length > 0
                  ? 'Some drive sessions are temporarily unavailable'
                  : 'Drive sessions temporarily unavailable'
              }
              detail={drivesResource.detail ?? 'The Hub drive route could not be read.'}
              kind="notice"
            />
          ))}
        {snapshot.drives.length === 0 ? (
          drivesResource.availability === 'temporarily-unavailable' ? null : (
            <DataState
              title="No recent drives"
              detail="The bounded result contains no drive sessions."
            />
          )
        ) : (
          <div className="session-list">
            {snapshot.drives.map((drive) => (
              <article className="session-card" key={drive.id}>
                <header>
                  <div>
                    <p className="card-kicker">{vehicleName(drive.vehicleId)}</p>
                    <h3>
                      {drive.startLabel ?? 'Unknown start'} →{' '}
                      {drive.endLabel ?? 'Unknown end'}
                    </h3>
                  </div>
                  {drive.quality === null ? (
                    <span className="status-pill" data-status="empty">
                      Quality not provided
                    </span>
                  ) : (
                    <QualityPill level={drive.quality.level} />
                  )}
                </header>
                <dl className="session-metrics">
                  <div>
                    <dt>Started</dt>
                    <dd>{formatDateTime(drive.startedAt)}</dd>
                  </div>
                  <div>
                    <dt>Distance</dt>
                    <dd>{valueOrAbsent(drive.distanceKm, ' km')}</dd>
                  </div>
                  <div>
                    <dt>Duration</dt>
                    <dd>{valueOrAbsent(drive.durationMinutes, ' min')}</dd>
                  </div>
                  <div>
                    <dt>Energy used</dt>
                    <dd>{valueOrAbsent(drive.energyUsedKwh, ' kWh')}</dd>
                  </div>
                </dl>
                <p className="evidence-line">
                  {drive.quality === null
                    ? 'Session quality is unsupported by this Hub profile.'
                    : `${drive.quality.sources.join(' + ')} · ${drive.quality.gapCount} gaps${
                        drive.quality.derivedFields.length > 0
                          ? ` · inferred: ${drive.quality.derivedFields.join(', ')}`
                          : ''
                      }`}
                </p>
              </article>
            ))}
          </div>
        )}
      </section>

      <section className="session-section" aria-labelledby="recent-charges-title">
        <div className="section-heading">
          <div>
            <p className="card-kicker">Energy</p>
            <h2 id="recent-charges-title">Recent charges</h2>
          </div>
          <span className="count-chip">{snapshot.charges.length}</span>
        </div>
        {snapshot.resources.charges.availability === 'unsupported' ? (
          <DataState
            title="Charge history unsupported"
            detail={snapshot.resources.charges.detail ?? 'Charge sessions are unsupported.'}
            kind="notice"
          />
        ) : snapshot.charges.length === 0 ? (
          <DataState
            title="No recent charges"
            detail="The bounded result contains no charging sessions."
          />
        ) : (
          <div className="session-list">
            {snapshot.charges.map((charge) => (
              <article className="session-card" key={charge.id}>
                <header>
                  <div>
                    <p className="card-kicker">{vehicleName(charge.vehicleId)}</p>
                    <h3>{charge.locationLabel ?? 'Location not reported'}</h3>
                  </div>
                  <QualityPill level={charge.quality.level} />
                </header>
                <dl className="session-metrics">
                  <div>
                    <dt>Started</dt>
                    <dd>{formatDateTime(charge.startedAt)}</dd>
                  </div>
                  <div>
                    <dt>Energy added</dt>
                    <dd>{valueOrAbsent(charge.energyAddedKwh, ' kWh')}</dd>
                  </div>
                  <div>
                    <dt>Start charge</dt>
                    <dd>{valueOrAbsent(charge.startPercent, '%')}</dd>
                  </div>
                  <div>
                    <dt>End charge</dt>
                    <dd>{valueOrAbsent(charge.endPercent, '%')}</dd>
                  </div>
                </dl>
                <p className="evidence-line">
                  {charge.quality.sources.join(' + ')} ·{' '}
                  {charge.quality.gapCount} gaps
                  {charge.quality.derivedFields.length > 0 &&
                    ` · inferred: ${charge.quality.derivedFields.join(', ')}`}
                </p>
              </article>
            ))}
          </div>
        )}
      </section>
    </ViewFrame>
  );
}
