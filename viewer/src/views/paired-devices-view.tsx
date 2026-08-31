import { useEffect, useRef, useState } from 'react';

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
  const [pendingDevice, setPendingDevice] = useState<PairedDevice | null>(null);
  const [removalError, setRemovalError] = useState<Error | null>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const removeButtons = useRef(new Map<string, HTMLButtonElement>());
  const restoreFocusDevice = useRef<string | null>(null);

  useEffect(() => {
    if (pendingDevice) {
      confirmButton.current?.focus();
      return;
    }

    if (restoreFocusDevice.current) {
      removeButtons.current.get(restoreFocusDevice.current)?.focus();
      restoreFocusDevice.current = null;
    }
  }, [pendingDevice]);

  function requestRemoval(device: PairedDevice) {
    setNotice('');
    setRemovalError(null);
    setPendingDevice(device);
  }

  function cancelRemoval() {
    restoreFocusDevice.current = pendingDevice?.id ?? null;
    setRemovalError(null);
    setPendingDevice(null);
  }

  async function remove(device: PairedDevice) {
    setBusyDevice(device.id);
    setNotice('');
    setRemovalError(null);
    try {
      await onRemove(device.id);
      setNotice(`${device.displayName} removed.`);
      setPendingDevice(null);
    } catch (error) {
      setRemovalError(
        error instanceof Error ? error : new Error('Device removal failed.'),
      );
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
                ) : pendingDevice?.id === device.id ? (
                  <div
                    role="group"
                    className="confirmation-panel"
                    aria-label={`Confirm removal of ${device.displayName}`}
                  >
                    <p>
                      Remove <strong>{device.displayName}</strong>?
                    </p>
                    <p>This removes only the fixture pairing shown here.</p>
                    {removalError && (
                      <p role="alert" className="inline-error">
                        Could not remove {device.displayName}.{' '}
                        {removalError.message}
                      </p>
                    )}
                    <div className="confirmation-actions">
                      <button
                        type="button"
                        className="secondary-button"
                        onClick={cancelRemoval}
                        disabled={busyDevice === device.id}
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        ref={confirmButton}
                        onClick={() => remove(device)}
                        disabled={busyDevice === device.id}
                      >
                        {busyDevice === device.id
                          ? `Removing ${device.displayName}…`
                          : removalError
                            ? `Try removal again for ${device.displayName}`
                            : `Confirm remove ${device.displayName}`}
                      </button>
                    </div>
                  </div>
                ) : (
                  <button
                    type="button"
                    className="secondary-button"
                    ref={(node) => {
                      if (node) removeButtons.current.set(device.id, node);
                      else removeButtons.current.delete(device.id);
                    }}
                    onClick={() => requestRemoval(device)}
                  >
                    Remove {device.displayName}
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
