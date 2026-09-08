import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { FixtureDataSource } from '../data/fixture-data-source';
import type { ViewerDataSource } from '../data/types';
import { ConnectionPanel } from './connection-panel';

const HUB_ID = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';

function liveSource(pair: ViewerDataSource['pair'] = vi.fn()) {
  return {
    configure: vi.fn(),
    discover: vi.fn(async () => [
      {
        id: HUB_ID,
        displayName: `Hub ${HUB_ID}`,
        endpoint: 'https://hub.example.test',
        manifestKey: null,
        tlsIdentity: null,
        protocolVersion: 'teslatlas-sync/1',
        status: 'healthy' as const,
      },
    ]),
    pair,
    readSnapshot: vi.fn(),
    loadMoreDrives: vi.fn(),
    removePairedDevice: vi.fn(),
    logout: vi.fn(async () => undefined),
  } satisfies ViewerDataSource;
}

describe('ConnectionPanel', () => {
  it('keeps invalid live endpoint input editable and focuses the error field', async () => {
    const user = userEvent.setup();
    const source = liveSource();
    render(<ConnectionPanel dataSource={source} mode="live" onPaired={vi.fn()} />);

    const endpoint = screen.getByLabelText('Hub endpoint');
    await user.type(endpoint, 'http://hub.example.test/path');
    await user.type(screen.getByLabelText('Expected Hub UUID'), HUB_ID);
    await user.click(screen.getByRole('button', { name: 'Inspect live Hub' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(/HTTPS/i);
    expect(endpoint).toHaveFocus();
    expect(source.configure).not.toHaveBeenCalled();
  });

  it('aborts a claim when editing and ignores its late completion', async () => {
    const user = userEvent.setup();
    let resolvePairing: ((value: Awaited<ReturnType<ViewerDataSource['pair']>>) => void) | undefined;
    const pair = vi.fn<ViewerDataSource['pair']>(
      (_input, signal) =>
        new Promise((resolve, reject) => {
          resolvePairing = resolve;
          signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
        }),
    );
    const source = liveSource(pair);
    const onPaired = vi.fn();
    render(<ConnectionPanel dataSource={source} mode="live" onPaired={onPaired} />);

    await user.type(screen.getByLabelText('Hub endpoint'), 'https://hub.example.test');
    await user.type(screen.getByLabelText('Expected Hub UUID'), HUB_ID);
    await user.click(screen.getByRole('button', { name: 'Inspect live Hub' }));
    await screen.findByRole('heading', { name: `Hub ${HUB_ID}` });
    fireEvent.change(screen.getByLabelText('Pairing invitation JSON'), {
      target: { value: '{}' },
    });
    await user.click(screen.getByRole('button', { name: 'Pair live Hub' }));
    expect(pair).toHaveBeenCalledWith(expect.anything(), expect.any(AbortSignal));

    await user.click(screen.getByRole('button', { name: 'Edit connection' }));
    expect(screen.getByLabelText('Hub endpoint')).toHaveFocus();
    resolvePairing?.({
      hubId: HUB_ID,
      deviceId: 'device-id',
      pairedAt: '2026-09-08T00:00:00.000Z',
      manifestKey: null,
      tlsIdentity: null,
    });
    await Promise.resolve();
    expect(onPaired).not.toHaveBeenCalled();
  });

  it('focuses the invitation after an accessible fixture pairing error', async () => {
    const user = userEvent.setup();
    const source = new FixtureDataSource();
    render(<ConnectionPanel dataSource={source} mode="fixture" onPaired={vi.fn()} />);
    await screen.findByRole('heading', { name: 'Hawthorn Hub' });
    const invitation = screen.getByLabelText('Invitation code');
    await user.type(invitation, '000000');
    await user.click(screen.getByRole('button', { name: 'Pair fixture Hub' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(/not valid/i);
    expect(invitation).toHaveFocus();
  });
});
