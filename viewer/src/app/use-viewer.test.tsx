import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { createFixtureSnapshot } from '../data/fixture-data';
import { ViewerDataError, type ViewerDataSource } from '../data/types';
import { useViewer } from './use-viewer';

type LoadMoreDrives = NonNullable<ViewerDataSource['loadMoreDrives']>;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function sourceWithReads(
  reads: ViewerDataSource['readSnapshot'],
  loadMoreDrives: LoadMoreDrives = vi.fn<LoadMoreDrives>(),
): ViewerDataSource {
  return {
    discover: vi.fn(),
    pair: vi.fn(),
    readSnapshot: reads,
    loadMoreDrives,
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

  it('cancels and ignores a late page when the hook is disabled', async () => {
    const first = createFixtureSnapshot('complete');
    const late = createFixtureSnapshot('stale');
    const page = deferred<ReturnType<typeof createFixtureSnapshot>>();
    let pageSignal: AbortSignal | undefined;
    const loadMoreDrives = vi.fn<LoadMoreDrives>((_vehicleId, signal) => {
      pageSignal = signal;
      return page.promise;
    });
    const source = sourceWithReads(
      vi.fn<ViewerDataSource['readSnapshot']>().mockResolvedValue(first),
      loadMoreDrives,
    );
    const { result, rerender } = renderHook(
      ({ enabled }: { enabled: boolean }) => useViewer(source, 'complete', enabled),
      { initialProps: { enabled: true } },
    );
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => {
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    expect(result.current.loadingDriveVehicleId).toBe('vehicle-redacted-1');

    rerender({ enabled: false });
    expect(pageSignal?.aborted).toBe(true);
    expect(result.current.phase).toBe('idle');
    expect(result.current.snapshot).toBeNull();
    expect(result.current.loadingDriveVehicleId).toBeNull();
    expect(result.current.drivePageError).toBeNull();

    await act(async () => {
      page.resolve(late);
      await page.promise;
    });
    expect(result.current.phase).toBe('idle');
    expect(result.current.snapshot).toBeNull();
  });

  it('cancels a pending page on unmount before it can publish', async () => {
    const first = createFixtureSnapshot('complete');
    const page = deferred<ReturnType<typeof createFixtureSnapshot>>();
    let pageSignal: AbortSignal | undefined;
    const loadMoreDrives = vi.fn<LoadMoreDrives>((_vehicleId, signal) => {
      pageSignal = signal;
      return page.promise;
    });
    const source = sourceWithReads(
      vi.fn<ViewerDataSource['readSnapshot']>().mockResolvedValue(first),
      loadMoreDrives,
    );
    const { result, unmount } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => {
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    unmount();
    expect(pageSignal?.aborted).toBe(true);

    await act(async () => {
      page.resolve(createFixtureSnapshot('stale'));
      await page.promise;
    });
    expect(loadMoreDrives).toHaveBeenCalledTimes(1);
  });

  it('cancels a late page when the data source changes', async () => {
    const first = createFixtureSnapshot('complete');
    const replacement = createFixtureSnapshot('inferred');
    const latePage = deferred<ReturnType<typeof createFixtureSnapshot>>();
    let pageSignal: AbortSignal | undefined;
    const sourceA = sourceWithReads(
      vi.fn<ViewerDataSource['readSnapshot']>().mockResolvedValue(first),
      vi.fn<LoadMoreDrives>((_vehicleId, signal) => {
        pageSignal = signal;
        return latePage.promise;
      }),
    );
    const sourceBRead = vi
      .fn<ViewerDataSource['readSnapshot']>()
      .mockResolvedValue(replacement);
    const sourceB = sourceWithReads(sourceBRead);
    const { result, rerender } = renderHook(
      ({ source }: { source: ViewerDataSource }) =>
        useViewer(source, 'complete', true),
      { initialProps: { source: sourceA } },
    );
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => {
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    rerender({ source: sourceB });
    expect(pageSignal?.aborted).toBe(true);
    await waitFor(() => expect(result.current.snapshot).toBe(replacement));

    await act(async () => {
      latePage.resolve(createFixtureSnapshot('stale'));
      await latePage.promise;
    });
    expect(result.current.snapshot).toBe(replacement);
    expect(sourceBRead).toHaveBeenCalledTimes(1);
  });

  it('serializes a refresh before a page and ignores duplicate page clicks', async () => {
    const first = createFixtureSnapshot('complete');
    const refreshed = createFixtureSnapshot('inferred');
    const refresh = deferred<ReturnType<typeof createFixtureSnapshot>>();
    const page = deferred<ReturnType<typeof createFixtureSnapshot>>();
    const reads = vi
      .fn<ViewerDataSource['readSnapshot']>()
      .mockResolvedValueOnce(first)
      .mockImplementationOnce(() => refresh.promise);
    const loadMoreDrives = vi
      .fn<LoadMoreDrives>()
      .mockImplementation(() => page.promise);
    const source = sourceWithReads(reads, loadMoreDrives);
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => {
      result.current.reload();
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    expect(reads).toHaveBeenCalledTimes(2);
    expect(loadMoreDrives).not.toHaveBeenCalled();
    expect(result.current.refreshing).toBe(true);

    await act(async () => {
      refresh.resolve(refreshed);
      await refresh.promise;
    });
    await waitFor(() => expect(result.current.snapshot).toBe(refreshed));

    act(() => {
      void result.current.loadMoreDrives('vehicle-redacted-1');
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    expect(loadMoreDrives).toHaveBeenCalledTimes(1);
    await act(async () => {
      page.resolve(createFixtureSnapshot('complete'));
      await page.promise;
    });
  });

  it('keeps unrelated resources when a page fails', async () => {
    const first = createFixtureSnapshot('complete');
    const loadMoreDrives = vi
      .fn<LoadMoreDrives>()
      .mockRejectedValue(new Error('history temporarily unavailable'));
    const source = sourceWithReads(
      vi.fn<ViewerDataSource['readSnapshot']>().mockResolvedValue(first),
      loadMoreDrives,
    );
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => {
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    await waitFor(() =>
      expect(result.current.drivePageError).toEqual({
        vehicleId: 'vehicle-redacted-1',
        message: 'history temporarily unavailable',
      }),
    );
    expect(result.current.snapshot).toBe(first);
    expect(result.current.snapshot?.resources.health.availability).toBe('present');
    expect(result.current.snapshot?.currentByVehicle['vehicle-redacted-2']).toEqual(
      first.currentByVehicle['vehicle-redacted-2'],
    );
  });

  it('clears every snapshot when a page reports lost authorization', async () => {
    const first = createFixtureSnapshot('complete');
    const loadMoreDrives = vi.fn<LoadMoreDrives>().mockRejectedValue(
      new ViewerDataError('AUTH_LOST', 'Hub authorization was lost.'),
    );
    const source = sourceWithReads(
      vi.fn<ViewerDataSource['readSnapshot']>().mockResolvedValue(first),
      loadMoreDrives,
    );
    const { result } = renderHook(() => useViewer(source, 'complete', true));
    await waitFor(() => expect(result.current.phase).toBe('ready'));

    act(() => {
      void result.current.loadMoreDrives('vehicle-redacted-1');
    });
    await waitFor(() => expect(result.current.phase).toBe('error'));
    expect(result.current.snapshot).toBeNull();
    expect(result.current.error).toMatchObject({ code: 'AUTH_LOST' });
    expect(result.current.loadingDriveVehicleId).toBeNull();
    expect(result.current.drivePageError).toBeNull();
  });
});
