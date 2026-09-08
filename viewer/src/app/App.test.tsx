import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { FixtureDataSource } from '../data/fixture-data-source';
import { createFixtureSnapshot } from '../data/fixture-data';
import type { ViewerDataSource } from '../data/types';
import { ViewerDataError } from '../data/types';

describe('App discovery and pairing', () => {
  it('offers live connection from the default fixture dashboard', () => {
    render(
      <App
        dataSource={new FixtureDataSource()}
        mode="fixture"
        initialPaired
      />,
    );

    expect(
      screen.getByRole('link', { name: /connect to my Hub/i }),
    ).toHaveAttribute('href', '/?mode=live&paired=false');
  });

  it('never treats a live URL or prop as already paired', () => {
    const readSnapshot = vi.fn(async () => createFixtureSnapshot('complete'));
    const source: ViewerDataSource = {
      discover: vi.fn(),
      pair: vi.fn(),
      readSnapshot,
      loadMoreDrives: vi.fn(async () => createFixtureSnapshot('complete')),
      removePairedDevice: vi.fn(),
      logout: vi.fn(async () => undefined),
    };

    render(<App dataSource={source} mode="live" initialPaired />);

    expect(screen.getByRole('heading', { name: 'Pair with a Hub' })).toBeInTheDocument();
    expect(readSnapshot).not.toHaveBeenCalled();
  });

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
        manifestKey: null,
        tlsIdentity: 'SHA256:4A:89:71:03:DE:MO',
        protocolVersion: 'fixture-0.1',
        status: 'healthy',
      },
      {
        id: 'hub-redacted-2',
        displayName: 'Birch Hub',
        endpoint: 'https://second.fixture.invalid',
        manifestKey: null,
        tlsIdentity: 'SHA256:7B:21:44:80:DE:MO',
        protocolVersion: 'fixture-0.1',
        status: 'healthy',
      },
    ]);
    const pairSpy = vi.spyOn(source, 'pair').mockImplementation(async (input) => ({
      hubId: input.hubId,
      deviceId: 'device-viewer',
      pairedAt: '2026-08-30T09:55:00.000Z',
      manifestKey: null,
      tlsIdentity: 'SHA256:7B:21:44:80:DE:MO',
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
      expect.any(AbortSignal),
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
          manifestKey: null,
          tlsIdentity: 'SHA256:4A:89:71:03:DE:MO',
          protocolVersion: 'fixture-0.1',
          status: 'healthy' as const,
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

  it('refreshes a paired snapshot without clearing the visible session', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    const readSnapshot = vi.spyOn(source, 'readSnapshot');
    render(<App dataSource={source} mode="fixture" initialPaired />);
    await screen.findByRole('region', { name: 'Hub health' });

    await user.click(screen.getByRole('button', { name: 'Refresh Hub data' }));

    expect(readSnapshot).toHaveBeenCalledTimes(2);
    expect(screen.getByRole('navigation', { name: 'Viewer sections' })).toBeVisible();
  });

  it('clears the in-memory viewer session and returns to discovery', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    const logout = vi.spyOn(source, 'logout');
    render(
      <App
        dataSource={source}
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
    expect(logout).toHaveBeenCalledOnce();
  });

  it('configures an exact live endpoint and expected Hub before claiming the invitation', async () => {
    const user = userEvent.setup();
    const configure = vi.fn();
    const discover = vi.fn(async () => [
      {
        id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        displayName: 'Hub aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        endpoint: 'https://localhost:18450',
        manifestKey: 'manifest-key',
        tlsIdentity: 'sha256:tls-certificate',
        protocolVersion: 'teslatlas-sync/1',
        status: 'healthy' as const,
      },
    ]);
    const pair = vi.fn(async () => ({
      hubId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      deviceId: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
      pairedAt: '2026-09-05T00:00:00.000Z',
      manifestKey: 'manifest-key',
      tlsIdentity: 'sha256:tls-certificate',
    }));
    const source: ViewerDataSource = {
      configure,
      discover,
      pair,
      readSnapshot: vi.fn(async () => createFixtureSnapshot('complete')),
      loadMoreDrives: vi.fn(async () => createFixtureSnapshot('complete')),
      removePairedDevice: vi.fn(),
      logout: vi.fn(async () => undefined),
    };

    render(
      <App
        dataSource={source}
        mode="live"
        initialPaired={false}
      />,
    );

    expect(screen.getByText(/credentials stay in memory/i)).toBeInTheDocument();
    await user.type(
      screen.getByLabelText('Hub endpoint'),
      'https://localhost:18450',
    );
    await user.type(
      screen.getByLabelText('Expected Hub UUID'),
      'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    );
    await user.type(
      screen.getByLabelText('Invitation TLS identity'),
      'sha256:tls-certificate',
    );
    await user.click(screen.getByRole('button', { name: 'Inspect live Hub' }));

    expect(configure).toHaveBeenCalledWith({
      endpoint: 'https://localhost:18450',
      expectedHubId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      tlsIdentity: 'sha256:tls-certificate',
    });
    expect(await screen.findByText('manifest-key')).toBeInTheDocument();

    const invitation = JSON.stringify({
      pairing_id: 'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
      secret: 'test-secret',
      expires_at_ms: 1_788_600_000_000,
      endpoint: 'https://localhost:18450',
      tls_pin: 'sha256:tls-certificate',
      pairing_uri: 'teslatlas://pair/test',
    });
    fireEvent.change(screen.getByLabelText('Pairing invitation JSON'), {
      target: { value: invitation },
    });
    await user.click(screen.getByRole('button', { name: 'Pair live Hub' }));

    expect(pair).toHaveBeenCalledWith(
      expect.objectContaining({
        hubId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
        invitationCode: invitation,
      }),
      expect.any(AbortSignal),
    );
  });

  it('lets a live user edit connection details after discovery without reloading', async () => {
    const user = userEvent.setup();
    const source: ViewerDataSource = {
      configure: vi.fn(),
      discover: vi.fn(async () => [
        {
          id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
          displayName: 'Hub aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
          endpoint: 'https://localhost:18450',
          manifestKey: null,
          tlsIdentity: null,
          protocolVersion: 'teslatlas-sync/1',
          status: 'healthy' as const,
        },
      ]),
      pair: vi.fn(),
      readSnapshot: vi.fn(async () => createFixtureSnapshot('complete')),
      loadMoreDrives: vi.fn(async () => createFixtureSnapshot('complete')),
      removePairedDevice: vi.fn(),
      logout: vi.fn(async () => undefined),
    };

    render(<App dataSource={source} mode="live" initialPaired={false} />);
    await user.type(screen.getByLabelText('Hub endpoint'), 'https://localhost:18450');
    await user.type(
      screen.getByLabelText('Expected Hub UUID'),
      'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    );
    await user.click(screen.getByRole('button', { name: 'Inspect live Hub' }));
    await screen.findByRole('heading', {
      name: 'Hub aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    });

    await user.click(screen.getByRole('button', { name: /edit connection/i }));

    expect(screen.getByLabelText('Hub endpoint')).toHaveValue('https://localhost:18450');
    expect(screen.getByLabelText('Expected Hub UUID')).toHaveValue(
      'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
    );
    expect(screen.getByRole('button', { name: 'Inspect live Hub' })).toBeInTheDocument();
  });

  it('returns to pairing when live authorization is lost', async () => {
    const user = userEvent.setup();
    const hubId = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
    const invitation = JSON.stringify({
      pairing_id: 'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
      secret: 'test-secret',
      expires_at_ms: 1_788_600_000_000,
      endpoint: 'https://localhost:18450',
      tls_pin: 'sha256:tls-certificate',
      pairing_uri: 'teslatlas://pair/test',
    });
    const source: ViewerDataSource = {
      configure: vi.fn(),
      discover: vi.fn(async () => [
        {
          id: hubId,
          displayName: `Hub ${hubId}`,
          endpoint: 'https://localhost:18450',
          manifestKey: null,
          tlsIdentity: null,
          protocolVersion: 'teslatlas-sync/1',
          status: 'healthy' as const,
        },
      ]),
      pair: vi.fn(async () => ({
        hubId,
        deviceId: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
        pairedAt: '2026-09-05T00:00:00.000Z',
        manifestKey: null,
        tlsIdentity: 'sha256:tls-certificate',
      })),
      readSnapshot: vi.fn(async () => {
        throw new ViewerDataError(
          'AUTH_LOST',
          'Hub authorization was lost. Pair this viewer again.',
        );
      }),
      loadMoreDrives: vi.fn(async () => createFixtureSnapshot('complete')),
      removePairedDevice: vi.fn(),
      logout: vi.fn(async () => undefined),
    };

    render(<App dataSource={source} mode="live" initialPaired />);

    await user.type(screen.getByLabelText('Hub endpoint'), 'https://localhost:18450');
    await user.type(screen.getByLabelText('Expected Hub UUID'), hubId);
    await user.click(screen.getByRole('button', { name: 'Inspect live Hub' }));
    await screen.findByRole('heading', { name: `Hub ${hubId}` });
    fireEvent.change(screen.getByLabelText('Pairing invitation JSON'), {
      target: { value: invitation },
    });
    await user.click(screen.getByRole('button', { name: 'Pair live Hub' }));

    expect(
      await screen.findByRole('heading', { name: 'Pair with a Hub' }),
    ).toBeInTheDocument();
    expect(source.logout).toHaveBeenCalledOnce();
  });
});
