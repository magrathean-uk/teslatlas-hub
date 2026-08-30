import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from '../app/App';
import { FixtureDataSource } from '../data/fixture-data-source';
import type { FixtureScenario } from '../data/types';

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

  it('removes a non-current paired device and announces the result', async () => {
    const user = userEvent.setup();
    renderScenario();

    const devices = await openView('Paired devices');
    await user.click(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
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

  it('preserves the active fixture scenario after removing a device', async () => {
    const user = userEvent.setup();
    renderScenario('stale');

    const devices = await openView('Paired devices');
    await user.click(
      within(devices).getByRole('button', { name: 'Remove Home automation' }),
    );
    await within(devices).findByText('Home automation removed.');

    const health = await openView('Hub health');
    expect(health).toHaveAttribute('data-view-state', 'stale');
  });
});
