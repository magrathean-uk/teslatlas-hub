import { useEffect, useState, type FormEvent } from 'react';

import type {
  AppMode,
  DiscoveredHub,
  PairedHub,
  ViewerDataSource,
} from '../data/types';

interface ConnectionPanelProps {
  dataSource: ViewerDataSource;
  mode: AppMode;
  onPaired: (pairing: PairedHub) => void;
}

type DiscoveryState =
  | { phase: 'discovering'; hubs: DiscoveredHub[]; error: null }
  | { phase: 'ready'; hubs: DiscoveredHub[]; error: null }
  | { phase: 'error'; hubs: DiscoveredHub[]; error: Error };

export function ConnectionPanel({
  dataSource,
  mode,
  onPaired,
}: ConnectionPanelProps) {
  const [discovery, setDiscovery] = useState<DiscoveryState>({
    phase: 'discovering',
    hubs: [],
    error: null,
  });
  const [deviceName, setDeviceName] = useState('Reference viewer');
  const [invitationCode, setInvitationCode] = useState('');
  const [selectedHubId, setSelectedHubId] = useState('');
  const [pairingError, setPairingError] = useState<Error | null>(null);
  const [isPairing, setIsPairing] = useState(false);
  const [discoveryAttempt, setDiscoveryAttempt] = useState(0);

  useEffect(() => {
    const controller = new AbortController();

    dataSource
      .discover(controller.signal)
      .then((hubs) => {
        if (!controller.signal.aborted) {
          setSelectedHubId(hubs[0]?.id ?? '');
          setDiscovery({ phase: 'ready', hubs, error: null });
        }
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) {
          setDiscovery({
            phase: 'error',
            hubs: [],
            error:
              error instanceof Error ? error : new Error('Hub discovery failed.'),
          });
        }
      });

    return () => controller.abort();
  }, [dataSource, discoveryAttempt]);

  const hub =
    discovery.hubs.find((candidate) => candidate.id === selectedHubId) ??
    discovery.hubs[0];

  function retryDiscovery() {
    setSelectedHubId('');
    setPairingError(null);
    setDiscovery({ phase: 'discovering', hubs: [], error: null });
    setDiscoveryAttempt((attempt) => attempt + 1);
  }

  async function submitPairing(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!hub) return;

    setIsPairing(true);
    setPairingError(null);
    try {
      const paired = await dataSource.pair({
        hubId: hub.id,
        invitationCode,
        deviceName,
      });
      onPaired(paired);
    } catch (error) {
      setPairingError(
        error instanceof Error ? error : new Error('Hub pairing failed.'),
      );
    } finally {
      setIsPairing(false);
    }
  }

  return (
    <main className="connection-layout" id="main-content">
      <section className="connection-panel" aria-labelledby="pairing-title">
        <p className="eyebrow">Public protocol reference</p>
        <h1 id="pairing-title">Pair with a Hub</h1>
        <p className="lede">
          Discover a Hub, confirm its identity, then use a scoped invitation.
          Fixture credentials stay in memory.
        </p>

        {mode === 'fixture' && (
          <p className="fixture-note">
            Demo only: use invitation code <strong>482731</strong>. No network
            request is made.
          </p>
        )}

        {discovery.phase === 'discovering' && (
          <div role="status" aria-label="Looking for Hubs" className="state-card">
            <span className="spinner" aria-hidden="true" />
            <span>Looking for Hubs…</span>
          </div>
        )}

        {discovery.phase === 'error' && (
          <div role="alert" className="state-card state-card--error">
            <strong>Hub discovery unavailable</strong>
            <span>{discovery.error.message}</span>
            <button type="button" onClick={retryDiscovery}>
              Retry discovery
            </button>
          </div>
        )}

        {discovery.phase === 'ready' && discovery.hubs.length === 0 && (
          <div role="status" className="state-card state-card--stacked">
            <strong>No Hubs found</strong>
            <span>Check the local connection and try discovery again.</span>
            <button type="button" onClick={retryDiscovery}>
              Retry discovery
            </button>
          </div>
        )}

        {hub && (
          <>
            <fieldset className="hub-selector">
              <legend>Select a discovered Hub</legend>
              <div className="hub-choice-list">
                {discovery.hubs.map((candidate, index) => {
                  const titleId = `discovered-hub-title-${index}`;
                  return (
                    <article
                      className="discovered-hub"
                      data-selected={candidate.id === hub.id}
                      key={candidate.id}
                    >
                      <div className="hub-choice-heading">
                        <input
                          type="radio"
                          name="discovered-hub"
                          value={candidate.id}
                          checked={candidate.id === hub.id}
                          onChange={() => setSelectedHubId(candidate.id)}
                          aria-labelledby={titleId}
                        />
                        <div>
                          <p className="card-kicker">Discovered Hub</p>
                          <h2 id={titleId}>{candidate.displayName}</h2>
                          <p>{candidate.endpoint}</p>
                        </div>
                      </div>
                      <dl className="identity-list">
                        <div>
                          <dt>Protocol</dt>
                          <dd>{candidate.protocolVersion}</dd>
                        </div>
                        <div>
                          <dt>Identity</dt>
                          <dd>{candidate.identityFingerprint}</dd>
                        </div>
                      </dl>
                    </article>
                  );
                })}
              </div>
            </fieldset>

            <form onSubmit={submitPairing} className="pairing-form">
              <p className="selected-hub-note">
                Pairing with <strong>{hub.displayName}</strong>
              </p>
              <label>
                Viewer name
                <input
                  value={deviceName}
                  onChange={(event) => setDeviceName(event.target.value)}
                  autoComplete="off"
                  required
                />
              </label>
              <label>
                Invitation code
                <input
                  value={invitationCode}
                  onChange={(event) => setInvitationCode(event.target.value)}
                  inputMode="numeric"
                  pattern="[0-9]{6}"
                  autoComplete="one-time-code"
                  required
                />
              </label>
              {pairingError && (
                <p role="alert" className="inline-error">
                  {pairingError.message}
                </p>
              )}
              <button type="submit" disabled={isPairing}>
                {isPairing ? 'Pairing…' : 'Pair fixture Hub'}
              </button>
            </form>
          </>
        )}
      </section>
    </main>
  );
}
