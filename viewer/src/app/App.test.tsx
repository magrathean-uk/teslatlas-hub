import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { createDataSource } from '../data/create-data-source';
import { FixtureDataSource } from '../data/fixture-data-source';

describe('App discovery and pairing', () => {
  it('finds the redacted fixture Hub and explains the fixture invitation', async () => {
    render(
      <App
        dataSource={new FixtureDataSource()}
        mode="fixture"
        initialPaired={false}
      />,
    );

    expect(
      screen.getByRole('status', { name: 'Looking for Hubs' }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole('heading', { name: 'Hawthorn Hub' }),
    ).toBeInTheDocument();
    expect(screen.getByText('482731')).toBeInTheDocument();
    expect(screen.getByText(/no network request is made/i)).toBeInTheDocument();
    expect(
      screen.getByText('https://hub.fixture.invalid'),
    ).toBeInTheDocument();
  });

  it('rejects the wrong invitation code with an accessible error', async () => {
    const user = userEvent.setup();
    render(
      <App
        dataSource={new FixtureDataSource()}
        mode="fixture"
        initialPaired={false}
      />,
    );

    await screen.findByRole('heading', { name: 'Hawthorn Hub' });
    await user.type(screen.getByLabelText('Invitation code'), '000000');
    await user.click(screen.getByRole('button', { name: 'Pair fixture Hub' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'That fixture invitation is not valid.',
    );
  });

  it('pairs in memory and exposes the pinned Hub identity', async () => {
    const user = userEvent.setup();
    render(
      <App
        dataSource={new FixtureDataSource()}
        mode="fixture"
        initialPaired={false}
      />,
    );

    await screen.findByRole('heading', { name: 'Hawthorn Hub' });
    await user.type(screen.getByLabelText('Invitation code'), '482731');
    await user.click(screen.getByRole('button', { name: 'Pair fixture Hub' }));

    expect(
      await screen.findByRole('heading', { name: 'Hub health' }),
    ).toBeInTheDocument();
    expect(screen.getByText('Pinned Hub identity')).toBeInTheDocument();
    expect(screen.getByText('SHA256:4A:89:71:03:DE:MO')).toBeInTheDocument();
  });

  it('lists every discovered Hub and pairs the selected identity', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    vi.spyOn(source, 'discover').mockResolvedValue([
      {
        id: 'hub-redacted-1',
        displayName: 'Hawthorn Hub',
        endpoint: 'https://hub.fixture.invalid',
        identityFingerprint: 'SHA256:4A:89:71:03:DE:MO',
        protocolVersion: 'fixture-0.1',
        status: 'healthy',
      },
      {
        id: 'hub-redacted-2',
        displayName: 'Birch Hub',
        endpoint: 'https://second.fixture.invalid',
        identityFingerprint: 'SHA256:7B:21:44:80:DE:MO',
        protocolVersion: 'fixture-0.1',
        status: 'healthy',
      },
    ]);
    const pairSpy = vi.spyOn(source, 'pair').mockImplementation(async (input) => ({
      hubId: input.hubId,
      deviceId: 'device-viewer',
      pairedAt: '2026-08-30T09:55:00.000Z',
      identityFingerprint: 'SHA256:7B:21:44:80:DE:MO',
    }));

    render(
      <App dataSource={source} mode="fixture" initialPaired={false} />,
    );

    expect(
      await screen.findByRole('heading', { name: 'Hawthorn Hub' }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('heading', { name: 'Birch Hub' }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole('radio', { name: 'Birch Hub' }));
    await user.type(screen.getByLabelText('Invitation code'), '482731');
    await user.click(screen.getByRole('button', { name: 'Pair fixture Hub' }));

    expect(pairSpy).toHaveBeenCalledWith(
      expect.objectContaining({ hubId: 'hub-redacted-2' }),
    );
  });

  it('retries empty and failed discovery without reloading the page', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    const discoverSpy = vi
      .spyOn(source, 'discover')
      .mockResolvedValueOnce([])
      .mockRejectedValueOnce(new Error('Temporary discovery failure.'))
      .mockResolvedValueOnce([
        {
          id: 'hub-redacted-1',
          displayName: 'Hawthorn Hub',
          endpoint: 'https://hub.fixture.invalid',
          identityFingerprint: 'SHA256:4A:89:71:03:DE:MO',
          protocolVersion: 'fixture-0.1',
          status: 'healthy',
        },
      ]);

    render(
      <App dataSource={source} mode="fixture" initialPaired={false} />,
    );

    expect(await screen.findByText('No Hubs found')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Retry discovery' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Temporary discovery failure.',
    );
    await user.click(screen.getByRole('button', { name: 'Retry discovery' }));
    expect(
      await screen.findByRole('heading', { name: 'Hawthorn Hub' }),
    ).toBeInTheDocument();
    expect(discoverSpy).toHaveBeenCalledTimes(3);
  });

  it('announces bounded loading and lets fixture users recover', async () => {
    const user = userEvent.setup();
    render(
      <App
        dataSource={new FixtureDataSource()}
        mode="fixture"
        initialPaired
        initialScenario="loading"
      />,
    );

    expect(
      screen.getByRole('status', { name: 'Loading Hub data' }),
    ).toHaveTextContent('Loading Hub data');

    await user.click(
      screen.getByRole('button', { name: 'Load complete fixture' }),
    );
    expect(
      await screen.findByRole('heading', { name: 'Hub health' }),
    ).toBeInTheDocument();
  });

  it('clears the in-memory viewer session and returns to discovery', async () => {
    const user = userEvent.setup();
    render(
      <App
        dataSource={new FixtureDataSource()}
        mode="fixture"
        initialPaired
      />,
    );

    expect(
      await screen.findByRole('region', { name: 'Hub health' }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole('button', { name: 'Clear local session' }),
    );

    expect(
      await screen.findByRole('heading', { name: 'Pair with a Hub' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('navigation', { name: 'Viewer sections' }),
    ).not.toBeInTheDocument();
  });

  it('explains unavailable live mode without making a network request', async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal('fetch', fetchSpy);

    render(
      <App
        dataSource={createDataSource('live')}
        mode="live"
        initialPaired={false}
      />,
    );

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Live Hub access needs a released Teslatlas TypeScript SDK.',
    );
    expect(fetchSpy).not.toHaveBeenCalled();

    vi.unstubAllGlobals();
  });
});
