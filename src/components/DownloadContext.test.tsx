import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../api/tauri';
import { DownloadProvider, useDownload } from './DownloadContext';

vi.mock('../api/tauri', () => ({
  onDownloadProgress: vi.fn(),
  downloadModel: vi.fn(),
  pauseDownload: vi.fn(),
  cancelDownload: vi.fn(),
}));

describe('Download lifecycle', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useDownload>;
  let progress: (event: api.DownloadProgressEvent) => void;
  let rejectDownload: (reason: string) => void;
  let pending: Promise<void>;
  function Harness() {
    const state = useDownload();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  async function start() {
    await act(async () => {
      renderer = create(
        <DownloadProvider>
          <Harness />
        </DownloadProvider>
      );
    });
    act(() => {
      pending = context.startDownload('publisher/model', ['Q4/model.gguf']);
    });
  }
  function emit(status: api.DownloadProgressEvent['status']) {
    progress({ fileName: 'model.gguf', downloaded: 50, total: 100, progress: 50, status });
  }
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.onDownloadProgress).mockImplementation(async (callback) => {
      progress = callback;
      return vi.fn<() => void>();
    });
    vi.mocked(api.downloadModel).mockImplementation(
      () =>
        new Promise((_, reject) => {
          rejectDownload = reject;
        })
    );
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('positive control: invokes download and retains shard paths on progress', async () => {
    await start();
    expect(api.downloadModel).toHaveBeenCalledExactlyOnceWith('publisher/model', ['Q4/model.gguf']);
    act(() => emit('downloading'));
    expect(context.activeDownload?.files).toEqual(['Q4/model.gguf']);
    expect(context.activeDownload?.progress).toBe(50);
  });

  it.each(['paused', 'cancelled'] as const)(
    'preserves %s when invoke rejects after its event',
    async (status) => {
      await start();
      await act(async () => {
        emit(status);
        rejectDownload(status);
        await pending;
      });
      expect(context.downloads.get('model.gguf')?.status).toBe(status);
      if (status === 'paused') {
        act(() => {
          vi.advanceTimersByTime(6000);
        });
        expect(context.downloads.get('model.gguf')?.files).toEqual(['Q4/model.gguf']);
        expect(context.downloads.get('model.gguf')?.repo).toBe('publisher/model');
      }
    }
  );

  it.each(['speed_test', 'installing'] as const)('treats %s as an active job', async (status) => {
    await start();
    act(() => emit(status));
    expect(context.isDownloading).toBe(true);
    expect(context.activeDownload?.status).toBe(status);
  });

  it('does not let a terminal cleanup timer delete a resumed job', async () => {
    await start();
    act(() => emit('error'));
    act(() => emit('downloading'));
    act(() => {
      vi.advanceTimersByTime(6000);
    });
    expect(context.downloads.get('model.gguf')?.status).toBe('downloading');
  });

  it('ignores a previous attempt rejection after resuming the same file', async () => {
    await start();
    const rejectPrevious = rejectDownload;
    const previous = pending;
    act(() => emit('paused'));
    act(() => {
      pending = context.startDownload('publisher/model', ['Q4/model.gguf']);
    });
    await act(async () => {
      rejectPrevious('Download paused');
      await previous;
    });
    expect(context.downloads.get('model.gguf')?.status).toBe('downloading');
    act(() => {
      vi.advanceTimersByTime(6000);
    });
    expect(context.downloads.get('model.gguf')?.status).toBe('downloading');
  });

  it('reports an invoke failure without an event and cleans it up', async () => {
    await start();
    await act(async () => {
      rejectDownload('Network unavailable');
      await pending;
    });
    expect(context.downloads.get('model.gguf')?.status).toBe('error');
    act(() => {
      vi.advanceTimersByTime(6000);
    });
    expect(context.downloads.size).toBe(0);
  });
});
