import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from '../app/App';
import { FixtureDataSource } from '../data/fixture-data-source';
import { createFixtureSnapshot } from '../data/fixture-data';
import type { DriveSummary, FixtureScenario } from '../data/types';
import { RecentSessionsView } from './recent-sessions-view';

function makeDrives(vehicleId: string, count: number): DriveSummary[] {
  const template = createFixtureSnapshot('complete').drives[0];
  return Array.from({ length: count }, (_, index) => ({
    ...template,
    id: `fixture-drive-${vehicleId}-${index + 1}`,
    vehicleId,
    startedAt: `2026-08-${String(30 - (index % 20)).padStart(2, '0')}T09:00:00.000Z`,
    endedAt: `2026-08-${String(30 - (index % 20)).padStart(2, '0')}T09:15:00.000Z`,
  }));
}

function renderScenario(scenario: FixtureScenario = 'complete') {
  return render(
    <App
      dataSource={new FixtureDataSource()}
      mode="fixture"
      initialPaired
      initialScenario={scenario}
    />,
  );
}

async function openView(name: string) {
  const user = userEvent.setup();
  const navigation = await screen.findByRole('navigation', {
    name: 'Viewer sections',
  });
  await user.click(within(navigation).getByRole('button', { name }));
  return screen.getByRole('region', { name });
}

describe('reference views', () => {
  it('distinguishes initial resource failures from successful empty results', async () => {
    const source = new FixtureDataSource();
    const snapshot = createFixtureSnapshot('complete');
    snapshot.vehicles = [];
    snapshot.currentByVehicle = {};
    snapshot.drives = [];
    snapshot.charges = [];
    snapshot.resources.vehicles = {
      availability: 'temporarily-unavailable',
      retained: false,
      detail: 'Vehicle route unavailable.',
    };
    snapshot.resources.current = {
      availability: 'temporarily-unavailable',
      retained: false,
      detail: 'Current route unavailable.',
    };
    snapshot.resources.drives = {
      availability: 'temporarily-unavailable',
      retained: false,
      detail: 'Drive route unavailable.',
    };
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const vehicles = await openView('Vehicles');
    expect(vehicles).toHaveAttribute('data-view-state', 'offline');
    expect(within(vehicles).getByText('Vehicles temporarily unavailable')).toBeInTheDocument();
    expect(within(vehicles).queryByText('No vehicles available')).not.toBeInTheDocument();

    const current = await openView('Current state');
    expect(current).toHaveAttribute('data-view-state', 'offline');
    expect(within(current).getByText('Current state temporarily unavailable')).toBeInTheDocument();
    expect(within(current).queryByText('No current state available')).not.toBeInTheDocument();

    const sessions = await openView('Recent sessions');
    expect(sessions).toHaveAttribute('data-view-state', 'offline');
    expect(within(sessions).getByText('Drive sessions temporarily unavailable')).toBeInTheDocument();
    expect(within(sessions).queryByText('No recent drives')).not.toBeInTheDocument();
  });

  it('keeps initial mixed current and drive results visible beside unavailable notices, then recovers', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    const mixed = createFixtureSnapshot('complete');
    mixed.currentByVehicle['vehicle-redacted-2'] = null;
    mixed.drives = mixed.drives.filter(
      (drive) => drive.vehicleId === 'vehicle-redacted-1',
    );
    mixed.resources.current = {
      availability: 'temporarily-unavailable',
      retained: false,
      detail: 'One vehicle current route is unavailable.',
    };
    mixed.resources.drives = {
      availability: 'temporarily-unavailable',
      retained: false,
      detail: 'One vehicle drive route is unavailable.',
    };
    const recovered = createFixtureSnapshot('complete');
    vi.spyOn(source, 'readSnapshot')
      .mockResolvedValueOnce(mixed)
      .mockResolvedValueOnce(recovered);
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const current = await openView('Current state');
    expect(
      within(current).getByText('Some current state is temporarily unavailable'),
    ).toBeInTheDocument();
    expect(
      within(current).getByRole('heading', { name: 'Northstar' }),
    ).toBeInTheDocument();
    expect(within(current).getByText('68%')).toBeInTheDocument();
    expect(
      within(current).queryByRole('heading', { name: 'Juniper' }),
    ).not.toBeInTheDocument();

    const sessions = await openView('Recent sessions');
    expect(
      within(sessions).getByText('Some drive sessions are temporarily unavailable'),
    ).toBeInTheDocument();
    expect(within(sessions).getByText('22.4 km')).toBeInTheDocument();
    expect(within(sessions).getAllByText('Northstar').length).toBeGreaterThan(0);

    await user.click(screen.getByRole('button', { name: 'Refresh Hub data' }));
    expect(
      await within(sessions).findByText('29.1 km'),
    ).toBeInTheDocument();
    expect(
      within(sessions).queryByText('Some drive sessions are temporarily unavailable'),
    ).not.toBeInTheDocument();

    const recoveredCurrent = await openView('Current state');
    expect(
      within(recoveredCurrent).getByRole('heading', { name: 'Juniper' }),
    ).toBeInTheDocument();
    expect(
      within(recoveredCurrent).queryByText('Some current state is temporarily unavailable'),
    ).not.toBeInTheDocument();
  });

  it('labels retained vehicle, current, and drive resources as stale', async () => {
    const source = new FixtureDataSource();
    const snapshot = createFixtureSnapshot('complete');
    for (const name of ['vehicles', 'current', 'drives'] as const) {
      snapshot.resources[name] = {
        availability: 'temporarily-unavailable',
        retained: true,
        detail: `${name} route unavailable`,
      };
    }
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const vehicles = await openView('Vehicles');
    expect(vehicles).toHaveAttribute('data-view-state', 'stale');
    expect(within(vehicles).getByText('Showing retained vehicles')).toBeInTheDocument();
    expect(within(vehicles).getByRole('heading', { name: 'Northstar' })).toBeInTheDocument();

    const current = await openView('Current state');
    expect(current).toHaveAttribute('data-view-state', 'stale');
    expect(within(current).getByText('Showing retained current state')).toBeInTheDocument();
    expect(within(current).getByText('68%')).toBeInTheDocument();

    const sessions = await openView('Recent sessions');
    expect(sessions).toHaveAttribute('data-view-state', 'stale');
    expect(within(sessions).getByText('Showing retained drive sessions')).toBeInTheDocument();
    expect(within(sessions).getByText('22.4 km')).toBeInTheDocument();
  });

  it('keeps healthy Hub status while identifying retained readiness', async () => {
    const source = new FixtureDataSource();
    const snapshot = createFixtureSnapshot('complete');
    snapshot.hub.readiness = 'not-ready';
    snapshot.hub.readinessReason = 'collector_stale';
    snapshot.resources.readiness = {
      availability: 'temporarily-unavailable',
      retained: true,
      detail: 'Readiness route unavailable.',
    };
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const health = await screen.findByRole('region', { name: 'Hub health' });
    expect(health).toHaveAttribute('data-view-state', 'complete');
    expect(within(health).getByText('Healthy')).toBeInTheDocument();
    expect(within(health).getByText('Showing retained readiness')).toBeInTheDocument();
    expect(within(health).getByText(/collector_stale/i)).toBeInTheDocument();
  });

  it('renders a failed health route as offline while retaining prior values', async () => {
    const source = new FixtureDataSource();
    const snapshot = createFixtureSnapshot('complete');
    snapshot.hub.status = 'offline';
    snapshot.hub.freshness = 'stale';
    snapshot.resources.health = {
      availability: 'temporarily-unavailable',
      retained: true,
      detail: 'Hub transport unavailable.',
    };
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const health = await screen.findByRole('region', { name: 'Hub health' });
    expect(health).toHaveAttribute('data-view-state', 'offline');
    expect(within(health).getByText('Cannot reach this Hub.')).toBeInTheDocument();
  });

  it('removes a retained-drive warning after a successful refresh', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    const retained = createFixtureSnapshot('complete');
    retained.resources.drives = {
      availability: 'temporarily-unavailable',
      retained: true,
      detail: 'Drive route unavailable.',
    };
    const recovered = createFixtureSnapshot('complete');
    vi.spyOn(source, 'readSnapshot')
      .mockResolvedValueOnce(retained)
      .mockResolvedValueOnce(recovered);
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const sessions = await openView('Recent sessions');
    expect(within(sessions).getByText('Showing retained drive sessions')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Refresh Hub data' }));
    expect(
      await within(sessions).findByText('Recent drives'),
    ).toBeInTheDocument();
    expect(within(sessions).queryByText('Showing retained drive sessions')).not.toBeInTheDocument();
    expect(sessions).toHaveAttribute('data-view-state', 'complete');
  });

  it('offers seven keyboard-operable sections with complete fixture data', async () => {
    const user = userEvent.setup();
    renderScenario();

    const navigation = await screen.findByRole('navigation', {
      name: 'Viewer sections',
    });
    expect(within(navigation).getAllByRole('button')).toHaveLength(7);

    const health = screen.getByRole('region', { name: 'Hub health' });
    expect(health).toHaveAttribute('data-view-state', 'complete');
    expect(within(health).getByText('Healthy')).toBeInTheDocument();
    expect(within(health).getByText('Pinned Hub identity')).toBeInTheDocument();

    await user.click(within(navigation).getByRole('button', { name: 'Vehicles' }));
    const vehicles = screen.getByRole('region', { name: 'Vehicles' });
    expect(within(vehicles).getByRole('heading', { name: 'Northstar' })).toBeInTheDocument();
    expect(within(vehicles).getByRole('heading', { name: 'Juniper' })).toBeInTheDocument();

    await user.click(
      within(navigation).getByRole('button', { name: 'Current state' }),
    );
    const current = screen.getByRole('region', { name: 'Current state' });
    expect(within(current).getByText('68%')).toBeInTheDocument();
    expect(within(current).getAllByText('Not reported').length).toBeGreaterThan(0);

    await user.click(
      within(navigation).getByRole('button', { name: 'Recent sessions' }),
    );
    const sessions = screen.getByRole('region', { name: 'Recent sessions' });
    expect(within(sessions).getByRole('heading', { name: 'Recent drives' })).toBeInTheDocument();
    expect(within(sessions).getByRole('heading', { name: 'Recent charges' })).toBeInTheDocument();

    await user.click(
      within(navigation).getByRole('button', { name: 'Data quality' }),
    );
    expect(
      within(screen.getByRole('region', { name: 'Data quality' })).getByText(
        'No unresolved gaps',
      ),
    ).toBeInTheDocument();

    await user.click(
      within(navigation).getByRole('button', { name: 'Collector freshness' }),
    );
    const freshness = screen.getByRole('region', {
      name: 'Collector freshness',
    });
    expect(within(freshness).getByText('48 seconds')).toBeInTheDocument();

    await user.click(
      within(navigation).getByRole('button', { name: 'Paired devices' }),
    );
    expect(
      within(screen.getByRole('region', { name: 'Paired devices' })).getByRole(
        'heading',
        { name: 'Home automation' },
      ),
    ).toBeInTheDocument();

    within(navigation).getByRole('button', { name: 'Hub health' }).focus();
    await user.keyboard('{Enter}');
    expect(
      within(navigation).getByRole('button', { name: 'Hub health' }),
    ).toHaveAttribute('aria-current', 'page');
  });

  it('distinguishes empty collections from absent fields', async () => {
    renderScenario('empty');

    const vehicles = await openView('Vehicles');
    expect(vehicles).toHaveAttribute('data-view-state', 'empty');
    expect(within(vehicles).getByText('No vehicles available')).toBeInTheDocument();

    const current = await openView('Current state');
    expect(current).toHaveAttribute('data-view-state', 'empty');
    expect(within(current).getByText('No current state available')).toBeInTheDocument();

    const sessions = await openView('Recent sessions');
    expect(within(sessions).getByText('No recent drives')).toBeInTheDocument();
    expect(within(sessions).getByText('No recent charges')).toBeInTheDocument();
  });

  it('shows per-vehicle counts and bounded continuation through 51 drives', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource({
      drives: makeDrives('vehicle-redacted-1', 51),
    });
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const sessions = await openView('Recent sessions');
    const group = within(sessions)
      .getByRole('heading', { name: 'Northstar' })
      .closest<HTMLElement>('.session-group');
    expect(group).not.toBeNull();
    if (!group) return;

    expect(within(group).getByText('25', { selector: '.count-chip' })).toBeInTheDocument();
    await user.click(
      within(group).getByRole('button', { name: 'Load more drives for Northstar' }),
    );
    expect(within(group).getByText('50', { selector: '.count-chip' })).toBeInTheDocument();
    await user.click(
      within(group).getByRole('button', { name: 'Load more drives for Northstar' }),
    );
    expect(within(group).getByText('51', { selector: '.count-chip' })).toBeInTheDocument();
    expect(within(group).queryByRole('button', { name: /Load more drives/ })).toBeNull();
    expect(within(group).getByText('End of available history')).toBeInTheDocument();
  });

  it('keeps an all-empty page actionable when history has continuation', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource({
      drivePages: {
        'vehicle-redacted-1': [[], makeDrives('vehicle-redacted-1', 1)],
      },
    });
    render(<App dataSource={source} mode="fixture" initialPaired />);

    const sessions = await openView('Recent sessions');
    const group = within(sessions)
      .getByRole('heading', { name: 'Northstar' })
      .closest<HTMLElement>('.session-group');
    expect(group).not.toBeNull();
    if (!group) return;
    expect(within(group).getByText('No recent drives for this vehicle.')).toBeInTheDocument();
    await user.click(
      within(group).getByRole('button', { name: 'Load more drives for Northstar' }),
    );
    expect(within(group).queryByText('No recent drives for this vehicle.')).toBeNull();
    expect(within(group).getByText('End of available history')).toBeInTheDocument();
  });

  it('separates unsupported, changed, and terminal history states', async () => {
    const source = new FixtureDataSource();
    const snapshot = createFixtureSnapshot('complete');
    snapshot.resources.drives = {
      availability: 'unsupported',
      retained: false,
      detail: 'Drive history is unsupported by this Hub profile.',
    };
    snapshot.drivePaging = Object.fromEntries(
      snapshot.vehicles.map((vehicle) => [
        vehicle.id,
        {
          resource: snapshot.resources.drives,
          hasMore: null,
          loadedCount: 0,
        },
      ]),
    );
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);
    render(<App dataSource={source} mode="fixture" initialPaired />);
    const sessions = await openView('Recent sessions');
    expect(within(sessions).getByText('Drive history is unsupported by this Hub profile.')).toBeInTheDocument();
    expect(within(sessions).queryByRole('button', { name: /Load more drives/ })).toBeNull();

  });

  it('shows history changed as refresh-only and omits a continuation control', () => {
    const snapshot = createFixtureSnapshot('complete');
    snapshot.drives = snapshot.drives.slice(0, 1);
    snapshot.drivePaging['vehicle-redacted-1'].hasMore = null;
    snapshot.drivePaging['vehicle-redacted-1'].loadedCount = 1;

    render(
      <RecentSessionsView
        snapshot={snapshot}
        state="complete"
        onLoadMore={vi.fn()}
      />,
    );

    const group = screen
      .getByRole('heading', { name: 'Northstar' })
      .closest<HTMLElement>('.session-group');
    expect(group).not.toBeNull();
    if (!group) return;
    expect(within(group).getByText('History changed; refresh to continue.')).toBeInTheDocument();
    expect(within(group).queryByRole('button', { name: /Load more drives/ })).toBeNull();
    expect(within(group).queryByText('End of available history')).toBeNull();
  });

  it('labels stale last-known data without presenting it as current', async () => {
    renderScenario('stale');

    const health = await screen.findByRole('region', { name: 'Hub health' });
    expect(health).toHaveAttribute('data-view-state', 'stale');
    expect(within(health).getByText('Stale')).toBeInTheDocument();
    expect(within(health).getByText(/last known data/i)).toBeInTheDocument();
  });

  it('labels inferred values and names each derived field', async () => {
    renderScenario('inferred');

    const current = await openView('Current state');
    expect(current).toHaveAttribute('data-view-state', 'inferred');
    expect(within(current).getAllByText('Inferred')).toHaveLength(2);
    expect(within(current).getByText('238 km')).toBeInTheDocument();
    expect(within(current).getByText('Estimated range is inferred')).toBeInTheDocument();
  });

  it('keeps derived charge fields beside their session values', async () => {
    const source = new FixtureDataSource();
    const snapshot = await source.readSnapshot('complete');
    snapshot.charges[0].quality.level = 'partial';
    snapshot.charges[0].quality.derivedFields = ['energyAddedKwh'];
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);

    render(
      <App
        dataSource={source}
        mode="fixture"
        initialPaired
        initialScenario="complete"
      />,
    );

    const sessions = await openView('Recent sessions');
    expect(sessions).toHaveAttribute('data-view-state', 'inferred');
    expect(
      within(sessions).getByText(/inferred: energyAddedKwh/i),
    ).toBeInTheDocument();
  });

  it('does not present partial session quality as complete', async () => {
    renderScenario('complete');

    const sessions = await openView('Recent sessions');
    const partialPills = within(sessions).getAllByText('Partial');
    expect(partialPills).toHaveLength(2);
    partialPills.forEach((pill) => {
      expect(pill).toHaveAttribute('data-status', 'degraded');
    });
  });

  it('labels unsupported live resources and does not offer remote device removal', async () => {
    const source = new FixtureDataSource();
    const snapshot = await source.readSnapshot('complete');
    snapshot.drives[0].quality = null;
    snapshot.charges = [];
    snapshot.quality = null;
    snapshot.collectors = [];
    snapshot.devices = [];
    snapshot.resources.charges = {
      availability: 'unsupported',
      retained: false,
      detail: 'Charge sessions are unsupported by hub-http-v1.',
    };
    snapshot.resources.quality = {
      availability: 'unsupported',
      retained: false,
      detail: 'Data quality is unsupported by hub-http-v1.',
    };
    snapshot.resources.collectors = {
      availability: 'unsupported',
      retained: false,
      detail: 'Collector cost and backup age are unsupported by hub-http-v1.',
    };
    snapshot.resources.devices = {
      availability: 'unsupported',
      retained: false,
      detail: 'Remote paired-device management is unsupported by hub-http-v1.',
    };
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);

    render(<App dataSource={source} mode="fixture" initialPaired />);

    const sessions = await openView('Recent sessions');
    expect(within(sessions).getByText('Quality not provided')).toBeInTheDocument();
    expect(
      within(sessions).getByText('Charge sessions are unsupported by hub-http-v1.'),
    ).toBeInTheDocument();

    const quality = await openView('Data quality');
    expect(
      within(quality).getByText('Data quality is unsupported by hub-http-v1.'),
    ).toBeInTheDocument();

    const freshness = await openView('Collector freshness');
    expect(
      within(freshness).getByText(
        'Collector cost and backup age are unsupported by hub-http-v1.',
      ),
    ).toBeInTheDocument();

    const devices = await openView('Paired devices');
    expect(
      within(devices).getByText(
        'Remote paired-device management is unsupported by hub-http-v1.',
      ),
    ).toBeInTheDocument();
    expect(within(devices).queryByRole('button', { name: /remove/i })).toBeNull();
  });

  it('surfaces degraded quality and every unresolved gap', async () => {
    renderScenario('degraded');

    const quality = await openView('Data quality');
    expect(quality).toHaveAttribute('data-view-state', 'degraded');
    expect(within(quality).getByText('Degraded')).toBeInTheDocument();
    expect(within(quality).getByText('2 unresolved gaps')).toBeInTheDocument();
    expect(within(quality).getByText('Telemetry delivery paused.')).toBeInTheDocument();
    expect(
      within(quality).getByText('API reconciliation unavailable.'),
    ).toBeInTheDocument();
  });

  it('marks mixed collector availability as degraded', async () => {
    const source = new FixtureDataSource();
    const snapshot = await source.readSnapshot('complete');
    snapshot.collectors[0].status = 'offline';
    snapshot.collectors[0].detail = 'This collection path is offline.';
    vi.spyOn(source, 'readSnapshot').mockResolvedValue(snapshot);

    render(
      <App
        dataSource={source}
        mode="fixture"
        initialPaired
        initialScenario="complete"
      />,
    );

    const freshness = await openView('Collector freshness');
    expect(freshness).toHaveAttribute('data-view-state', 'degraded');
    expect(within(freshness).getByText('offline')).toBeInTheDocument();
    expect(within(freshness).getByText('healthy')).toBeInTheDocument();
  });

  it('keeps offline separate from stale and degraded', async () => {
    renderScenario('offline');

    const health = await screen.findByRole('region', { name: 'Hub health' });
    expect(health).toHaveAttribute('data-view-state', 'offline');
    expect(within(health).getByText('Offline')).toBeInTheDocument();
    expect(within(health).getByText(/cannot reach this Hub/i)).toBeInTheDocument();

    const vehicles = await openView('Vehicles');
    expect(vehicles).toHaveAttribute('data-view-state', 'offline');
    expect(within(vehicles).getAllByText('offline')).toHaveLength(2);
    expect(within(vehicles).queryByText('online')).not.toBeInTheDocument();

    const current = await openView('Current state');
    expect(current).toHaveAttribute('data-view-state', 'offline');
    expect(within(current).getAllByText('Stale')).toHaveLength(2);
    expect(within(current).queryByText('Observed')).not.toBeInTheDocument();

    const quality = await openView('Data quality');
    expect(quality).toHaveAttribute('data-view-state', 'offline');

    const devices = await openView('Paired devices');
    expect(devices).toHaveAttribute('data-view-state', 'offline');
  });

  it('shows typed source errors as an error state with retry', async () => {
    const user = userEvent.setup();
    renderScenario('error');

    const alert = await screen.findByRole('alert');
    expect(alert.closest('main')).toHaveAttribute('data-view-state', 'error');
    expect(alert).toHaveTextContent('The fixture Hub could not be read.');
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
    await user.click(
      screen.getByRole('button', { name: 'Load complete fixture' }),
    );
    expect(
      await screen.findByRole('region', { name: 'Hub health' }),
    ).toHaveAttribute('data-view-state', 'complete');
  });

  it('requires named device-removal confirmation and restores focus on cancel', async () => {
    const user = userEvent.setup();
    renderScenario();

    const devices = await openView('Paired devices');
    await user.click(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
    );

    expect(
      within(devices).getByRole('heading', { name: 'Home automation' }),
    ).toBeInTheDocument();
    expect(
      within(devices).getByRole('button', {
        name: 'Confirm remove Home automation',
      }),
    ).toHaveFocus();

    await user.click(within(devices).getByRole('button', { name: 'Cancel' }));
    expect(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
    ).toHaveFocus();
  });

  it('removes a non-current paired device and announces the result', async () => {
    const user = userEvent.setup();
    renderScenario();

    const devices = await openView('Paired devices');
    await user.click(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
    );
    await user.click(
      within(devices).getByRole('button', {
        name: 'Confirm remove Home automation',
      }),
    );

    expect(
      await within(devices).findByText('Home automation removed.'),
    ).toBeInTheDocument();
    expect(
      within(devices).queryByRole('heading', { name: 'Home automation' }),
    ).not.toBeInTheDocument();
    expect(
      within(devices).getByText('Current fixture device'),
    ).toBeInTheDocument();
  });

  it('preserves a device after removal failure and allows retry', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    const removeDevice = source.removePairedDevice.bind(source);
    vi.spyOn(source, 'removePairedDevice')
      .mockImplementation(removeDevice)
      .mockRejectedValueOnce(new Error('Fixture removal denied.'));

    render(
      <App
        dataSource={source}
        mode="fixture"
        initialPaired
        initialScenario="complete"
      />,
    );

    const devices = await openView('Paired devices');
    await user.click(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
    );
    await user.click(
      within(devices).getByRole('button', {
        name: 'Confirm remove Home automation',
      }),
    );

    expect(await within(devices).findByRole('alert')).toHaveTextContent(
      'Fixture removal denied.',
    );
    expect(
      within(devices).getByRole('heading', { name: 'Home automation' }),
    ).toBeInTheDocument();

    await user.click(
      within(devices).getByRole('button', {
        name: 'Try removal again for Home automation',
      }),
    );
    expect(
      await within(devices).findByText('Home automation removed.'),
    ).toBeInTheDocument();
  });

  it('preserves the active fixture scenario after removing a device', async () => {
    const user = userEvent.setup();
    renderScenario('stale');

    const devices = await openView('Paired devices');
    await user.click(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
    );
    await user.click(
      within(devices).getByRole('button', {
        name: 'Confirm remove Home automation',
      }),
    );
    await within(devices).findByText('Home automation removed.');

    const health = await openView('Hub health');
    expect(health).toHaveAttribute('data-view-state', 'stale');
  });
});
