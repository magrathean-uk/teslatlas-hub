import { DataState } from '../components/data-state';
import { StatusPill, type ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { HubSnapshot } from '../data/types';
import { formatDateTime } from './view-helpers';

interface VehiclesViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
}

export function VehiclesView({ snapshot, state }: VehiclesViewProps) {
  return (
    <ViewFrame
      id="vehicles"
      eyebrow="Visible resources"
      title="Vehicles"
      description="Vehicles exposed to this paired device and their last reported state."
      state={state}
    >
      {snapshot.vehicles.length === 0 ? (
        <DataState
          title="No vehicles available"
          detail="The Hub returned an empty vehicle collection for this device."
        />
      ) : (
        <div className="card-grid">
          {snapshot.vehicles.map((vehicle) => (
            <article className="resource-card" key={vehicle.id}>
              <header>
                <div>
                  <p className="card-kicker">{vehicle.visibility} visibility</p>
                  <h2>{vehicle.displayName}</h2>
                </div>
                <StatusPill
                  state={
                    vehicle.state === 'offline'
                      ? 'offline'
                      : vehicle.freshness === 'stale'
                        ? 'stale'
                        : 'complete'
                  }
                  label={vehicle.state}
                />
              </header>
              <dl className="compact-list">
                <div>
                  <dt>Reference</dt>
                  <dd className="mono-text">{vehicle.id}</dd>
                </div>
                <div>
                  <dt>Updated</dt>
                  <dd>{formatDateTime(vehicle.updatedAt)}</dd>
                </div>
              </dl>
            </article>
          ))}
        </div>
      )}
    </ViewFrame>
  );
}
