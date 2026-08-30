import { useState } from 'react';

import { DataState } from '../components/data-state';
import { StatusPill, type ViewState } from '../components/status-pill';
import { ViewFrame } from '../components/view-frame';
import type { HubSnapshot, PairedDevice } from '../data/types';
import { formatDateTime } from './view-helpers';

interface PairedDevicesViewProps {
  snapshot: HubSnapshot;
  state: ViewState;
  onRemove: (deviceId: string) => Promise<void>;
}

export function PairedDevicesView({
  snapshot,
  state,
  onRemove,
}: PairedDevicesViewProps) {
  const [busyDevice, setBusyDevice] = useState<string | null>(null);
  const [notice, setNotice] = useState('');

  async function remove(device: PairedDevice) {
    setBusyDevice(device.id);
    setNotice('');
    try {
      await onRemove(device.id);
      setNotice(`${device.displayName} removed.`);
    } finally {
      setBusyDevice(null);
    }
  }

  return (
    <ViewFrame
      id="paired-devices"
      eyebrow="Scoped access"
      title="Paired devices"
      description="Review visible device scopes and remove a fixture pairing."
      state={state}
    >
      <p className="live-notice" aria-live="polite">
        {notice}
      </p>
      {snapshot.devices.length === 0 ? (
        <DataState
          title="No paired devices"
          detail="The Hub returned an empty paired-device collection."
        />
      ) : (
        <div className="device-list">
          {snapshot.devices.map((device) => {
            const isCurrent = device.id === 'device-viewer';
            return (
              <article className="device-card" key={device.id}>
                <header>
                  <div>
                    <p className="card-kicker">{device.kind}</p>
                    <h2>{device.displayName}</h2>
                  </div>
                  <StatusPill
                    state={device.status === 'active' ? 'complete' : 'stale'}
                    label={device.status}
                  />
                </header>
                <dl className="compact-list">
                  <div>
                    <dt>Paired</dt>
                    <dd>{formatDateTime(device.pairedAt)}</dd>
                  </div>
                  <div>
                    <dt>Last seen</dt>
                    <dd>{formatDateTime(device.lastSeenAt)}</dd>
                  </div>
                </dl>
                <ul className="tag-list" aria-label={`${device.displayName} scopes`}>
                  {device.scopes.map((scope) => (
                    <li key={scope}>{scope}</li>
                  ))}
                </ul>
                {isCurrent ? (
                  <p className="current-device">Current fixture device</p>
                ) : (
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={() => remove(device)}
                    disabled={busyDevice === device.id}
                  >
                    {busyDevice === device.id
                      ? `Removing ${device.displayName}…`
                      : `Remove ${device.displayName}`}
                  </button>
                )}
              </article>
            );
          })}
        </div>
      )}
    </ViewFrame>
  );
}
