// AI-career dashboard selection and explicit refresh signal.

import { create } from 'zustand';
import type {
  AiCareerFamily,
  DailyTokenEntry,
  DailyTokenResponse,
  HeatmapEntry,
  SavedSession,
} from '../api/aiCareer';

type Scope = AiCareerFamily | 'overview';

interface CareerSnapshot {
  dashboard?: { heatmapEntries: HeatmapEntry[]; tokenBytes: number };
  dailyTokens?: DailyTokenResponse;
  sessions?: SavedSession[];
}

const STORAGE_KEY = 'echobird-ai-career-cache';
const CACHE_VERSION = 1;
const SCOPES: Scope[] = ['overview', 'claude', 'codex', 'opencode', 'hermes', 'mimo', 'deepseek'];
const numeric = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value) && value >= 0;
const nullableNumber = (value: unknown) => value === null || numeric(value);
const nullableString = (value: unknown) => value === null || typeof value === 'string';

function persistedSnapshots(snapshots: Partial<Record<Scope, CareerSnapshot>>) {
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return Object.fromEntries(
    Object.entries(snapshots).map(([scope, snapshot]) => {
      if (!snapshot.dailyTokens) return [scope, snapshot];
      const days = new Map<string, DailyTokenEntry>();
      for (const entry of snapshot.dailyTokens.entries) {
        const day = new Date(entry.ts * 1000).toDateString();
        const previous = days.get(day);
        days.set(day, {
          ts: previous?.ts ?? entry.ts,
          input_tokens: (previous?.input_tokens ?? 0) + entry.input_tokens,
          output_tokens: (previous?.output_tokens ?? 0) + entry.output_tokens,
        });
      }
      return [
        scope,
        {
          ...snapshot,
          dailyTokens: { ...snapshot.dailyTokens, entries: [...days.values()], timeZone },
        },
      ];
    })
  );
}

function readSnapshots(): Partial<Record<Scope, CareerSnapshot>> {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? 'null');
    if (saved?.version !== CACHE_VERSION || !saved.data || typeof saved.data !== 'object')
      return {};
    const snapshots: Partial<Record<Scope, CareerSnapshot>> = {};
    for (const scope of SCOPES) {
      const value = saved.data[scope];
      if (!value || typeof value !== 'object') continue;
      const snapshot: CareerSnapshot = {};
      const dashboard = value.dashboard;
      if (
        dashboard &&
        numeric(dashboard.tokenBytes) &&
        Array.isArray(dashboard.heatmapEntries) &&
        dashboard.heatmapEntries.every(
          (entry: HeatmapEntry) => entry && numeric(entry.ts) && numeric(entry.count)
        )
      )
        snapshot.dashboard = dashboard;
      const tokens = value.dailyTokens;
      if (
        tokens &&
        tokens.timeZone === Intl.DateTimeFormat().resolvedOptions().timeZone &&
        Array.isArray(tokens.entries) &&
        tokens.entries.every(
          (entry: DailyTokenResponse['entries'][number]) =>
            entry &&
            numeric(entry.ts) &&
            numeric(entry.input_tokens) &&
            numeric(entry.output_tokens)
        ) &&
        Array.isArray(tokens.unavailable_families) &&
        tokens.unavailable_families.every(
          (family: unknown) =>
            typeof family === 'string' && family !== 'overview' && SCOPES.includes(family as Scope)
        )
      )
        snapshot.dailyTokens = {
          entries: tokens.entries,
          unavailable_families: tokens.unavailable_families,
        };
      if (
        scope !== 'overview' &&
        Array.isArray(value.sessions) &&
        value.sessions.every(
          (session: SavedSession) =>
            session &&
            ['id', 'name', 'tool', 'cwd', 'saved_at'].every(
              (key) => typeof session[key as keyof SavedSession] === 'string'
            ) &&
            nullableString(session.session_token) &&
            nullableString(session.file_path) &&
            (session.model_id === undefined || nullableString(session.model_id)) &&
            (session.message_count === undefined || nullableNumber(session.message_count)) &&
            nullableNumber(session.turn_count) &&
            nullableNumber(session.input_tokens) &&
            nullableNumber(session.output_tokens)
        )
      )
        snapshot.sessions = value.sessions.slice(0, 20);
      if (Object.keys(snapshot).length) snapshots[scope] = snapshot;
    }
    return snapshots;
  } catch {
    return {};
  }
}

interface AiCareerState {
  selectedFamily: AiCareerFamily | null;
  setSelectedFamily: (family: AiCareerFamily | null) => void;
  chartMetric: 'messages' | 'tokens';
  setChartMetric: (metric: 'messages' | 'tokens') => void;
  refreshKey: number;
  refreshing: boolean;
  refresh: () => void;
  setRefreshing: (value: boolean) => void;
  cachedScopes: Partial<Record<Scope, CareerSnapshot>>;
  cacheDashboard: (
    family: AiCareerFamily | null,
    entries: HeatmapEntry[],
    tokenBytes: number
  ) => void;
  cacheDailyTokens: (family: AiCareerFamily | null, data: DailyTokenResponse) => void;
  cacheSessions: (family: AiCareerFamily, sessions: SavedSession[]) => void;
}

export const useAiCareerStore = create<AiCareerState>((set) => {
  const restored = readSnapshots();
  let savedPayload = JSON.stringify({ version: CACHE_VERSION, data: persistedSnapshots(restored) });
  const cache = <K extends keyof CareerSnapshot>(
    family: AiCareerFamily | null,
    field: K,
    value: CareerSnapshot[K]
  ) =>
    set((state) => {
      const scope = family ?? 'overview';
      if (JSON.stringify(state.cachedScopes[scope]?.[field]) === JSON.stringify(value))
        return state;
      const cachedScopes = {
        ...state.cachedScopes,
        [scope]: { ...state.cachedScopes[scope], [field]: value },
      };
      try {
        const payload = JSON.stringify({
          version: CACHE_VERSION,
          data: persistedSnapshots(cachedScopes),
        });
        if (payload !== savedPayload) {
          localStorage.setItem(STORAGE_KEY, payload);
          savedPayload = payload;
        }
      } catch {
        // A storage failure must not discard the current in-memory snapshot.
      }
      return { cachedScopes };
    });
  return {
    selectedFamily: null,
    setSelectedFamily: (family) => set({ selectedFamily: family }),
    chartMetric: 'tokens',
    setChartMetric: (metric) => set({ chartMetric: metric }),
    refreshKey: 0,
    refreshing: false,
    refresh: () => set((s) => ({ refreshKey: s.refreshKey + 1, refreshing: true })),
    setRefreshing: (value) => set({ refreshing: value }),
    cachedScopes: restored,
    cacheDashboard: (family, heatmapEntries, tokenBytes) =>
      cache(family, 'dashboard', { heatmapEntries, tokenBytes }),
    cacheDailyTokens: (family, data) => cache(family, 'dailyTokens', data),
    cacheSessions: (family, sessions) => cache(family, 'sessions', sessions.slice(0, 20)),
  };
});
