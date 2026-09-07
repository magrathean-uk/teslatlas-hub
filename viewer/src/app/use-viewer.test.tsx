import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { createFixtureSnapshot } from '../data/fixture-data';
import { ViewerDataError, type ViewerDataSource } from '../data/types';
import { useViewer } from './use-viewer';

function sourceWithReads(
  reads: ViewerDataSource['readSnapshot'],
): ViewerDataSource {
  return {
    discover: vi.fn(),
    pair: vi.fn(),
    readSnapshot: reads,
    removePairedDevice: vi.fn(),
    logout: vi.fn(async () => undefined),
  };
}

describe('useViewer retained state', () => {
  it('replaces a resource-specific retained snapshot when that route recovers', async () => {
    const retained = createFixtureSnapshot('complete');
    retained.resources.drives = {
      availability: 'temporarily-unavailable',
      retained: true,
      detail: 'Drive route unavailable.',
    };
    const recovered = createFixtureSnapshot('complete');
    const source = sourceWithReads(
      vi
        .fn<ViewerDataSource['readSnapshot']>()
        .mockResolvedValueOnce(retained)
        .mockResolvedValueOnce(recovered),
    );
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() =>
      expect(result.current.snapshot?.resources.drives.retained).toBe(true),
    );

    act(() => result.current.reload());

    await waitFor(() =>
      expect(result.current.snapshot?.resources.drives.availability).toBe(
        'present',
      ),
    );
    expect(result.current.snapshot?.resources.drives.retained).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it('keeps the previous snapshot visible while a reload is pending', async () => {
    let resolveReload:
      | ((value: ReturnType<typeof createFixtureSnapshot>) => void)
      | undefined;
    const first = createFixtureSnapshot('complete');
    const second = createFixtureSnapshot('stale');
    const source = sourceWithReads(
      vi
        .fn<ViewerDataSource['readSnapshot']>()
        .mockResolvedValueOnce(first)
        .mockImplementationOnce(
          () =>
            new Promise((resolve) => {
              resolveReload = resolve;
            }),
        ),
    );
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => result.current.reload());

    expect(result.current.phase).toBe('ready');
    expect(result.current.snapshot).toBe(first);
    expect(result.current.refreshing).toBe(true);
    resolveReload?.(second);
    await waitFor(() => expect(result.current.snapshot).toBe(second));
  });

  it('retains the previous snapshot as visibly stale after a reload error', async () => {
    const first = createFixtureSnapshot('complete');
    const source = sourceWithReads(
      vi
        .fn<ViewerDataSource['readSnapshot']>()
        .mockResolvedValueOnce(first)
        .mockRejectedValueOnce(new Error('temporary outage')),
    );
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => result.current.reload());

    await waitFor(() => expect(result.current.error?.message).toBe('temporary outage'));
    expect(result.current.phase).toBe('ready');
    expect(result.current.snapshot?.hub.freshness).toBe('stale');
    expect(
      Object.values(result.current.snapshot?.currentByVehicle ?? {}).every(
        (value) => value === null || value.freshness === 'stale',
      ),
    ).toBe(true);
  });

  it('drops retained identity-bound data when authorization is lost', async () => {
    const first = createFixtureSnapshot('complete');
    const source = sourceWithReads(
      vi
        .fn<ViewerDataSource['readSnapshot']>()
        .mockResolvedValueOnce(first)
        .mockRejectedValueOnce(
          new ViewerDataError(
            'AUTH_LOST',
            'Hub authorization was lost. Pair this viewer again.',
          ),
        ),
    );
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => result.current.reload());

    await waitFor(() => expect(result.current.phase).toBe('error'));
    expect(result.current.snapshot).toBeNull();
    expect(result.current.error).toMatchObject({ code: 'AUTH_LOST' });
  });
});
