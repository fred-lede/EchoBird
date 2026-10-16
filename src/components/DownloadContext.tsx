// Global download state management Context
// Ported from Electron v1.1.0 → Tauri API
import React, { createContext, useContext, useState, useEffect, useCallback, useRef } from 'react';
import * as api from '../api/tauri';

// Download progress data structure
export interface DownloadItem {
  fileName: string;
  repo?: string; // Repository name, used for resuming after pause
  /** Full file list for the variant (single-element for single-file GGUFs;
   *  all shards in order for sharded GGUFs). Kept so resume-after-pause and
   *  cancel-after-pause can hand the backend the right names. */
  files?: string[];
  progress: number;
  downloaded: number;
  total: number;
  status:
    | 'downloading'
    | 'completed'
    | 'error'
    | 'cancelled'
    | 'paused'
    | 'speed_test'
    | 'installing';
  /** 1-based shard counter (multi-shard only). */
  shardIndex?: number;
  /** Total shards in the variant (multi-shard only). */
  shardCount?: number;
}

interface DownloadContextValue {
  // All download items
  downloads: Map<string, DownloadItem>;
  // Whether there is any active downloading item
  isDownloading: boolean;
  // The currently active download (the first 'downloading' status)
  activeDownload: DownloadItem | null;
  // Trigger download (also used for resuming after pause). Pass the variant's
  // full `files` array — single-element for single-file GGUFs, multi-element
  // for sharded GGUFs. The first entry becomes the progress map's primary key.
  startDownload: (repo: string, files: string[]) => Promise<void>;
  // Pause download (saves progress, resumable)
  pauseDownload: () => void;
  // Cancel download. Pass the variant's `files` so cancel-after-pause can
  // wipe every shard's temp file; omit to wipe whatever the backend is
  // currently tracking.
  cancelDownload: (files?: string[]) => void;
}

const DownloadContext = createContext<DownloadContextValue | null>(null);

export const useDownload = () => {
  const ctx = useContext(DownloadContext);
  if (!ctx) throw new Error('useDownload must be used within DownloadProvider');
  return ctx;
};

export const DownloadProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [downloads, setDownloads] = useState<Map<string, DownloadItem>>(new Map());
  // Auto-cleanup timer refs
  const cleanupTimers = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  const downloadStatuses = useRef<Map<string, DownloadItem['status']>>(new Map());
  const downloadAttempt = useRef(0);

  // Listen to Tauri download progress events
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    api
      .onDownloadProgress((data) => {
        downloadStatuses.current.set(data.fileName, data.status);
        const previousTimer = cleanupTimers.current.get(data.fileName);
        if (previousTimer) {
          clearTimeout(previousTimer);
          cleanupTimers.current.delete(data.fileName);
        }
        setDownloads((prev) => {
          const next = new Map(prev);
          const existing = prev.get(data.fileName);
          next.set(data.fileName, {
            fileName: data.fileName,
            repo: existing?.repo, // Keep existing repo info
            files: existing?.files, // Preserve shard list set at startDownload
            progress: data.progress,
            downloaded: data.downloaded,
            total: data.total,
            status: data.status as DownloadItem['status'],
            shardIndex: data.shardIndex,
            shardCount: data.shardCount,
          });
          return next;
        });

        // Completed, error or cancelled items auto-cleanup after 5s (paused not cleaned, status preserved)
        if (data.status === 'completed' || data.status === 'error' || data.status === 'cancelled') {
          // Clear previous timer if any (prevent duplicates)
          const existing = cleanupTimers.current.get(data.fileName);
          if (existing) clearTimeout(existing);

          const timer = setTimeout(() => {
            setDownloads((prev) => {
              const next = new Map(prev);
              next.delete(data.fileName);
              return next;
            });
            cleanupTimers.current.delete(data.fileName);
            downloadStatuses.current.delete(data.fileName);
          }, 5000);
          cleanupTimers.current.set(data.fileName, timer);
        }
      })
      .then((fn) => {
        // If the effect already cleaned up before this resolved, unlisten now
        // so the listener isn't leaked (StrictMode mounts/unmounts fast).
        if (cancelled) fn();
        else unlisten = fn;
      });

    const timers = cleanupTimers.current;
    return () => {
      cancelled = true;
      unlisten?.();
      // Cleanup all timers
      timers.forEach((t) => clearTimeout(t));
      timers.clear();
    };
  }, []);

  // Trigger download (also used for resuming after pause). The Map key must
  // match what the backend emits in DownloadProgressEvent.fileName — the
  // backend reports against the *basename* of files[0] (HF paths like
  // "UD-IQ2_M/foo-00001-of-00003.gguf" are flattened on save), so the UI
  // placeholder must use the same basename or progress events land in a
  // separate entry and the 0%-downloading row never updates.
  const startDownload = useCallback(async (repo: string, files: string[]) => {
    if (!files.length) return;
    if (
      Array.from(downloadStatuses.current.values()).some((status) =>
        ['downloading', 'speed_test', 'installing'].includes(status)
      )
    )
      return;
    const head = files[0];
    const attempt = ++downloadAttempt.current;
    const slash = head.lastIndexOf('/');
    const primary = slash === -1 ? head : head.slice(slash + 1);
    downloadStatuses.current.set(primary, 'downloading');
    // A prior completed/cancelled download of the same basename may still have
    // a pending 5s cleanup timer; cancel it so it can't delete this fresh entry.
    const pendingCleanup = cleanupTimers.current.get(primary);
    if (pendingCleanup) {
      clearTimeout(pendingCleanup);
      cleanupTimers.current.delete(primary);
    }
    // Immediately show downloading state
    setDownloads((prev) => {
      const next = new Map(prev);
      next.set(primary, {
        fileName: primary,
        repo, // Store repo for resume after pause
        files, // Store full shard list for cancel-after-pause
        progress: 0,
        downloaded: 0,
        total: 0,
        status: 'downloading',
        shardCount: files.length > 1 ? files.length : undefined,
        shardIndex: files.length > 1 ? 1 : undefined,
      });
      return next;
    });
    try {
      await api.downloadModel(repo, files);
    } catch (e) {
      if (attempt !== downloadAttempt.current) return;
      if (
        ['paused', 'cancelled', 'completed'].includes(downloadStatuses.current.get(primary) || '')
      ) {
        return;
      }
      downloadStatuses.current.set(primary, 'error');
      console.error('[DownloadContext] Download failed:', e);
      // The invoke rejected before any progress event — transition the optimistic
      // row to 'error' and schedule the same 5s cleanup the progress path uses,
      // so it doesn't sit at 0% 'downloading' forever.
      setDownloads((prev) => {
        const next = new Map(prev);
        const existing = prev.get(primary);
        if (existing) next.set(primary, { ...existing, status: 'error' });
        return next;
      });
      const prevTimer = cleanupTimers.current.get(primary);
      if (prevTimer) clearTimeout(prevTimer);
      const timer = setTimeout(() => {
        setDownloads((prev) => {
          const next = new Map(prev);
          next.delete(primary);
          return next;
        });
        cleanupTimers.current.delete(primary);
        downloadStatuses.current.delete(primary);
      }, 5000);
      cleanupTimers.current.set(primary, timer);
    }
  }, []);

  // Pause download (keeps .downloading temp file, resumable)
  const pauseDownload = useCallback(async () => {
    try {
      await api.pauseDownload();
    } catch (e) {
      console.error('[DownloadContext] Pause failed:', e);
    }
  }, []);

  // Cancel download (deletes .downloading temp files; for multi-shard pass
  // all shard names so cancel-after-pause cleans every leftover)
  const cancelDownload = useCallback(async (files?: string[]) => {
    try {
      await api.cancelDownload(files);
    } catch (e) {
      console.error('[DownloadContext] Cancel failed:', e);
    }
  }, []);

  // Derived values (memoized to prevent unnecessary re-renders)
  const isDownloading = React.useMemo(
    () =>
      Array.from(downloads.values()).some((d) =>
        ['downloading', 'speed_test', 'installing'].includes(d.status)
      ),
    [downloads]
  );
  const activeDownload = React.useMemo(
    () =>
      Array.from(downloads.values()).find((d) =>
        ['downloading', 'speed_test', 'installing'].includes(d.status)
      ) || null,
    [downloads]
  );

  return (
    <DownloadContext.Provider
      value={{
        downloads,
        isDownloading,
        activeDownload,
        startDownload,
        pauseDownload,
        cancelDownload,
      }}
    >
      {children}
    </DownloadContext.Provider>
  );
};
