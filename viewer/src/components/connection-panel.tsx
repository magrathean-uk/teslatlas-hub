import {
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from 'react';

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
  notice?: string | null;
}

type DiscoveryState =
  | { phase: 'idle'; hubs: DiscoveredHub[]; error: null }
  | { phase: 'discovering'; hubs: DiscoveredHub[]; error: null }
  | { phase: 'ready'; hubs: DiscoveredHub[]; error: null }
  | { phase: 'error'; hubs: DiscoveredHub[]; error: Error };

export function ConnectionPanel({
  dataSource,
  mode,
  onPaired,
  notice = null,
}: ConnectionPanelProps) {
  const [discovery, setDiscovery] = useState<DiscoveryState>({
    phase: mode === 'fixture' ? 'discovering' : 'idle',
    hubs: [],
    error: null,
  });
  const [deviceName, setDeviceName] = useState('Reference viewer');
  const [invitationCode, setInvitationCode] = useState('');
  const [selectedHubId, setSelectedHubId] = useState('');
  const [pairingError, setPairingError] = useState<Error | null>(null);
  const [isPairing, setIsPairing] = useState(false);
  const [discoveryAttempt, setDiscoveryAttempt] = useState(0);
  const [endpoint, setEndpoint] = useState('');
  const [expectedHubId, setExpectedHubId] = useState('');
  const [tlsIdentity, setTlsIdentity] = useState('');
  const [connectionError, setConnectionError] = useState<Error | null>(null);
  const pairingController = useRef<AbortController | null>(null);
  const endpointInput = useRef<HTMLInputElement>(null);
  const expectedHubInput = useRef<HTMLInputElement>(null);
  const invitationInput = useRef<HTMLInputElement>(null);
  const invitationTextarea = useRef<HTMLTextAreaElement>(null);
  const focusEndpointAfterEdit = useRef(false);

  useEffect(() => {
    if (mode === 'live' && discoveryAttempt === 0) return;
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
  }, [dataSource, discoveryAttempt, mode]);

  useEffect(() => {
    if (mode === 'live' && discovery.phase === 'idle' && focusEndpointAfterEdit.current) {
      focusEndpointAfterEdit.current = false;
      endpointInput.current?.focus();
    }
  }, [discovery.phase, mode]);

  useEffect(() => {
    if (pairingError !== null) {
      invitationInput.current?.focus();
      invitationTextarea.current?.focus();
    }
  }, [pairingError]);

  useEffect(
    () => () => {
      pairingController.current?.abort();
      pairingController.current = null;
    },
    [],
  );

  const hub =
    discovery.hubs.find((candidate) => candidate.id === selectedHubId) ??
    discovery.hubs[0];

  function retryDiscovery() {
    setConnectionError(null);
    setSelectedHubId('');
    setPairingError(null);
    setDiscovery({ phase: 'discovering', hubs: [], error: null });
    setDiscoveryAttempt((attempt) => attempt + 1);
  }

  function editConnection() {
    pairingController.current?.abort();
    pairingController.current = null;
    setIsPairing(false);
    setSelectedHubId('');
    setInvitationCode('');
    setConnectionError(null);
    setPairingError(null);
    setDiscovery({ phase: 'idle', hubs: [], error: null });
    setDiscoveryAttempt(0);
    focusEndpointAfterEdit.current = true;
  }

  function validateLiveConnection(): Error | null {
    const value = endpoint.trim();
    if (value.length === 0) {
      return new Error('Enter the root HTTPS origin for the Hub.');
    }
    try {
      const parsed = new URL(value);
      if (parsed.protocol !== 'https:') {
        return new Error('Live Hub endpoints must use HTTPS.');
      }
      if (
        parsed.username.length > 0 ||
        parsed.password.length > 0 ||
        parsed.pathname !== '/' ||
        parsed.search.length > 0 ||
        parsed.hash.length > 0
      ) {
        return new Error(
          'Enter the root HTTPS origin without a path, query, or fragment.',
        );
      }
    } catch {
      return new Error('Enter a valid HTTPS Hub origin.');
    }
    if (expectedHubId.trim().length === 0) {
      return new Error('Enter the expected Hub UUID.');
    }
    return null;
  }

  function inspectLiveHub(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSelectedHubId('');
    setPairingError(null);
    const validationError = validateLiveConnection();
    if (validationError !== null) {
      setConnectionError(validationError);
      if (validationError.message.includes('UUID')) {
        expectedHubInput.current?.focus();
      } else {
        endpointInput.current?.focus();
      }
      return;
    }
    setConnectionError(null);
    setDiscovery({ phase: 'discovering', hubs: [], error: null });
    try {
      dataSource.configure?.({
        endpoint: endpoint.trim(),
        expectedHubId: expectedHubId.trim(),
        tlsIdentity: tlsIdentity.trim() || null,
      });
      setDiscoveryAttempt((attempt) => attempt + 1);
    } catch (error) {
      setDiscovery({
        phase: 'error',
        hubs: [],
        error:
          error instanceof Error ? error : new Error('Hub configuration failed.'),
      });
    }
  }

  async function submitPairing(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!hub) return;

    setIsPairing(true);
    setPairingError(null);
    const controller = new AbortController();
    pairingController.current = controller;
    try {
      const paired = await dataSource.pair({
        hubId: hub.id,
        invitationCode,
        deviceName,
      }, controller.signal);
      if (!controller.signal.aborted && pairingController.current === controller) {
        onPaired(paired);
      }
    } catch (error) {
      if (!controller.signal.aborted) {
        setPairingError(
          error instanceof Error ? error : new Error('Hub pairing failed.'),
        );
      }
    } finally {
      if (pairingController.current === controller) {
        pairingController.current = null;
        setIsPairing(false);
      }
    }
  }

  return (
    <main className="connection-layout" id="main-content">
      <section className="connection-panel" aria-labelledby="pairing-title">
        <p className="eyebrow">Public protocol reference</p>
        <h1 id="pairing-title">Pair with a Hub</h1>
        {notice && (
          <div className="notice-banner" data-tone="stale" role="status" tabIndex={-1}>
            <strong>Access ended; pair again</strong>
            <span>{notice}</span>
          </div>
        )}
        <p className="lede">
          {mode === 'fixture'
            ? 'Discover a fixture Hub, confirm its identity, then use a scoped invitation. Fixture credentials stay in memory.'
            : 'Inspect an exact HTTPS Hub identity, then claim a scoped invitation. Live credentials stay in memory and disappear when this session is cleared.'}
        </p>

        {mode === 'fixture' && (
          <p className="fixture-note">
            Demo only: use invitation code <strong>482731</strong>. No network
            request is made.
          </p>
        )}

        {mode === 'live' && (
          <p className="fixture-note">
            Connecting to a Hub uses its browser-reachable HTTPS origin.{' '}
            <a href="/?mode=fixture">View the local demo instead.</a>
          </p>
        )}

        {mode === 'live' && discovery.phase === 'idle' && (
          <form onSubmit={inspectLiveHub} className="pairing-form">
            <label>
              Hub endpoint
              <input
                ref={endpointInput}
                type="url"
                value={endpoint}
                onChange={(event) => setEndpoint(event.target.value)}
                placeholder="https://hub.example:443"
                autoComplete="off"
                required
              />
            </label>
            <label>
              Expected Hub UUID
              <input
                ref={expectedHubInput}
                value={expectedHubId}
                onChange={(event) => setExpectedHubId(event.target.value)}
                autoComplete="off"
                required
              />
            </label>
            <label>
              Invitation TLS identity
              <input
                value={tlsIdentity}
                onChange={(event) => setTlsIdentity(event.target.value)}
                autoComplete="off"
                placeholder="Optional until invitation claim"
              />
            </label>
            {connectionError && (
              <p role="alert" className="inline-error">
                {connectionError.message}
              </p>
            )}
            <button type="submit">Inspect live Hub</button>
          </form>
        )}

        {discovery.phase === 'discovering' && (
          <div role="status" aria-label="Looking for Hubs" className="state-card">
            <span className="spinner" aria-hidden="true" />
            <span>Looking for Hubs…</span>
            {mode === 'live' && (
              <button type="button" onClick={editConnection}>
                Edit connection
              </button>
            )}
          </div>
        )}

        {discovery.phase === 'error' && (
          <div role="alert" className="state-card state-card--error">
            <strong>Hub discovery unavailable</strong>
            <span>{discovery.error.message}</span>
            <button type="button" onClick={retryDiscovery}>
              Retry discovery
            </button>
            {mode === 'live' && (
              <button type="button" onClick={editConnection}>
                Edit connection
              </button>
            )}
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
                          <dt>Hub UUID</dt>
                          <dd>{candidate.id}</dd>
                        </div>
                        <div>
                          <dt>Manifest key</dt>
                          <dd>{candidate.manifestKey ?? 'Not advertised'}</dd>
                        </div>
                        <div>
                          <dt>TLS identity</dt>
                          <dd>{candidate.tlsIdentity ?? 'Provided by invitation'}</dd>
                        </div>
                      </dl>
                    </article>
                  );
                })}
              </div>
            </fieldset>

            <form onSubmit={submitPairing} className="pairing-form">
              {mode === 'live' && (
                <button type="button" className="secondary-button" onClick={editConnection}>
                  Edit connection
                </button>
              )}
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
              {mode === 'fixture' ? (
                <label>
                  Invitation code
                  <input
                    ref={invitationInput}
                    value={invitationCode}
                    onChange={(event) => setInvitationCode(event.target.value)}
                    inputMode="numeric"
                    pattern="[0-9]{6}"
                    autoComplete="one-time-code"
                    required
                  />
                </label>
              ) : (
                <label>
                  Pairing invitation JSON
                  <textarea
                    ref={invitationTextarea}
                    value={invitationCode}
                    onChange={(event) => setInvitationCode(event.target.value)}
                    autoComplete="off"
                    rows={7}
                    required
                  />
                </label>
              )}
              {pairingError && (
                <p role="alert" className="inline-error">
                  {pairingError.message}
                </p>
              )}
              <button type="submit" disabled={isPairing}>
                {isPairing
                  ? 'Pairing…'
                  : mode === 'fixture'
                    ? 'Pair fixture Hub'
                    : 'Pair live Hub'}
              </button>
            </form>
          </>
        )}
      </section>
    </main>
  );
}
