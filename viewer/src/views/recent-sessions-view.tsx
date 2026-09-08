import { DataState } from '../components/data-state';
import type { ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { DriveSummary, HubSnapshot } from '../data/types';
import {
  formatDateTime,
  QualityPill,
  valueOrAbsent,
} from './view-helpers';

interface RecentSessionsViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
  onLoadMore?: (vehicleId: string) => void;
  loadingVehicleId?: string | null;
  pageError?: { vehicleId: string; message: string } | null;
  /** Refresh and page operations share one serialized hook guard. */
  pagingDisabled?: boolean;
}

function driveKey(drive: DriveSummary): string {
  return `${drive.vehicleId}:${drive.id}`;
}

function uniqueDrives(drives: readonly DriveSummary[]): DriveSummary[] {
  const seen = new Set<string>();
  return drives.filter((drive) => {
    const key = driveKey(drive);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function RecentSessionsView({
  snapshot,
  state,
  onLoadMore,
  loadingVehicleId = null,
  pageError = null,
  pagingDisabled = false,
}: RecentSessionsViewProps) {
  const drivesResource = snapshot.resources.drives;
  const vehicleName = (vehicleId: string) =>
    snapshot.vehicles.find((vehicle) => vehicle.id === vehicleId)?.displayName ??
    'Unknown vehicle';
  const drives = uniqueDrives(snapshot.drives);
  const vehicleIds = Array.from(
    new Set([
      ...snapshot.vehicles.map((vehicle) => vehicle.id),
      ...drives.map((drive) => drive.vehicleId),
    ]),
  );

  const renderDriveGroups =
    drivesResource.availability === 'present' ||
    drives.length > 0 ||
    vehicleIds.length > 0;

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
          <span className="count-chip" aria-label={`${drives.length} drives`}>
            {drives.length}
          </span>
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
                drives.length > 0
                  ? 'Some drive sessions are temporarily unavailable'
                  : 'Drive sessions temporarily unavailable'
              }
              detail={drivesResource.detail ?? 'The Hub drive route could not be read.'}
              kind="notice"
            />
          ))}

        {drivesResource.availability === 'unsupported' ? (
          <DataState
            title="Drive history unsupported"
            detail={
              drivesResource.detail ??
              'Drive history is unsupported by this Hub profile.'
            }
            kind="notice"
          />
        ) : drives.length === 0 && drivesResource.availability === 'present' ? (
          <DataState
            title="No recent drives"
            detail="The bounded result contains no drive sessions."
          />
        ) : null}

        {renderDriveGroups && drivesResource.availability !== 'unsupported' && (
          <div className="session-groups">
            {vehicleIds.map((vehicleId) => {
              const vehicleDrives = drives.filter(
                (drive) => drive.vehicleId === vehicleId,
              );
              const paging = snapshot.drivePaging[vehicleId];
              const pagingResource = paging?.resource ?? drivesResource;
              const hasPageError = pageError?.vehicleId === vehicleId;
              const canLoadMore =
                !hasPageError &&
                pagingResource.availability === 'present' &&
                paging?.hasMore === true &&
                onLoadMore !== undefined;
              const isLoading = loadingVehicleId === vehicleId;
              const showHistoryChanged =
                pagingResource.availability === 'present' &&
                paging?.hasMore === null;
              const showEnd =
                pagingResource.availability === 'present' &&
                paging?.hasMore === false;

              return (
                <section
                  className="session-group"
                  key={vehicleId}
                  aria-labelledby={`drives-${vehicleId}`}
                >
                  <div className="section-heading session-group-heading">
                    <div>
                      <p className="card-kicker">Vehicle history</p>
                      <h3 id={`drives-${vehicleId}`}>{vehicleName(vehicleId)}</h3>
                    </div>
                    <span
                      className="count-chip"
                      aria-label={`${paging?.loadedCount ?? vehicleDrives.length} loaded drives for ${vehicleName(vehicleId)}`}
                    >
                      {paging?.loadedCount ?? vehicleDrives.length}
                    </span>
                  </div>

                  {pagingResource.availability === 'temporarily-unavailable' && (
                    <DataState
                      title={
                        pagingResource.retained
                          ? 'Showing retained drive history'
                          : 'Drive history unavailable'
                      }
                      detail={
                        pagingResource.detail ??
                        'This vehicle history could not be read.'
                      }
                      kind="notice"
                    />
                  )}

                  {hasPageError && (
                    <div className="notice-banner" data-tone="stale" role="status">
                      <strong>More drive history unavailable</strong>
                      <span>{pageError.message}</span>
                      {onLoadMore && (
                        <button
                          type="button"
                          onClick={() => onLoadMore(vehicleId)}
                          disabled={pagingDisabled}
                        >
                          {isLoading ? 'Retrying history…' : 'Retry history'}
                        </button>
                      )}
                    </div>
                  )}

                  {canLoadMore && (
                    <button
                      type="button"
                      className="secondary-button history-action"
                      onClick={() => onLoadMore(vehicleId)}
                      disabled={pagingDisabled}
                      aria-busy={isLoading}
                    >
                      {isLoading
                        ? 'Loading more drives…'
                        : `Load more drives for ${vehicleName(vehicleId)}`}
                    </button>
                  )}

                  {showHistoryChanged && (
                    <p className="history-terminal" role="status">
                      History changed; refresh to continue.
                    </p>
                  )}

                  {vehicleDrives.length === 0 &&
                    pagingResource.availability === 'present' &&
                    !showHistoryChanged &&
                    !showEnd && (
                      <p className="empty-group-note">No recent drives for this vehicle.</p>
                    )}

                  {vehicleDrives.length > 0 && (
                    <div className="session-list">
                      {vehicleDrives.map((drive) => (
                        <article className="session-card" key={driveKey(drive)}>
                          <header>
                            <div>
                              <h4>
                                {drive.startLabel ?? 'Unknown start'} →{' '}
                                {drive.endLabel ?? 'Unknown end'}
                              </h4>
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

                  {showEnd && (
                    <p className="history-terminal" role="status">
                      End of available history
                    </p>
                  )}
                </section>
              );
            })}
          </div>
        )}
      </section>

      <section className="session-section" aria-labelledby="recent-charges-title">
        <div className="section-heading">
          <div>
            <p className="card-kicker">Energy</p>
            <h2 id="recent-charges-title">Recent charges</h2>
          </div>
          <span className="count-chip" aria-label={`${snapshot.charges.length} charges`}>
            {snapshot.charges.length}
          </span>
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
              <article className="session-card" key={`${charge.vehicleId}:${charge.id}`}>
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
                  {charge.quality.sources.join(' + ')} · {charge.quality.gapCount} gaps
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
