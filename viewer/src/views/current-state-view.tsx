import { DataState } from '../components/data-state';
import { Metric } from '../components/metric';
import { StatusPill, type ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { HubSnapshot } from '../data/types';
import { formatDateTime, valueOrAbsent } from './view-helpers';

interface CurrentStateViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
}

const fieldLabels: Record<string, string> = {
  estimatedRangeKm: 'Estimated range',
  stateOfChargePercent: 'State of charge',
  odometerKm: 'Odometer',
  locationLabel: 'Location',
  locked: 'Lock state',
  insideTemperatureC: 'Inside temperature',
  outsideTemperatureC: 'Outside temperature',
};

export function CurrentStateView({
  snapshot,
  state,
}: CurrentStateViewProps) {
  const available = snapshot.vehicles.filter(
    (vehicle) => snapshot.currentByVehicle[vehicle.id],
  );

  return (
    <ViewFrame
      id="current-state"
      eyebrow="Latest projection"
      title="Current state"
      description="Observed values stay separate from absent and inferred values."
      state={state}
    >
      {available.length === 0 ? (
        <DataState
          title="No current state available"
          detail="Vehicles may exist, but the Hub reported no current-state projection."
        />
      ) : (
        <div className="state-stack">
          {available.map((vehicle) => {
            const current = snapshot.currentByVehicle[vehicle.id]!;
            return (
              <article className="panel vehicle-state" key={vehicle.id}>
                <header>
                  <div>
                    <p className="card-kicker">Updated {formatDateTime(current.updatedAt)}</p>
                    <h2>{vehicle.displayName}</h2>
                  </div>
                  <StatusPill
                    state={
                      current.freshness === 'stale'
                        ? 'stale'
                        : current.inferredFields.length > 0
                          ? 'inferred'
                          : 'complete'
                    }
                    label={
                      current.freshness === 'stale'
                        ? 'Stale'
                        : current.inferredFields.length > 0
                          ? 'Inferred'
                          : 'Observed'
                    }
                  />
                </header>
                <dl className="metric-grid metric-grid--state">
                  <Metric
                    label="State of charge"
                    value={valueOrAbsent(current.stateOfChargePercent, '%')}
                  />
                  <Metric
                    label="Estimated range"
                    value={valueOrAbsent(current.estimatedRangeKm, ' km')}
                  />
                  <Metric
                    label="Odometer"
                    value={valueOrAbsent(current.odometerKm?.toLocaleString('en-GB') ?? null, ' km')}
                  />
                  <Metric
                    label="Location"
                    value={valueOrAbsent(current.locationLabel)}
                  />
                  <Metric
                    label="Lock state"
                    value={
                      current.locked === null
                        ? valueOrAbsent(null)
                        : current.locked
                          ? 'Locked'
                          : 'Unlocked'
                    }
                  />
                  <Metric
                    label="Outside"
                    value={valueOrAbsent(current.outsideTemperatureC, '°C')}
                  />
                </dl>
                {current.inferredFields.length > 0 && (
                  <div className="inference-note">
                    <strong>Inferred fields</strong>
                    <ul>
                      {current.inferredFields.map((field) => (
                        <li key={field}>
                          {fieldLabels[field] ?? field} is inferred
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </article>
            );
          })}
        </div>
      )}
    </ViewFrame>
  );
}
