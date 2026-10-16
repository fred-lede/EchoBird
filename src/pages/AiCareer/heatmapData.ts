// Pure helpers for the contribution heatmap + the five summary stats.
// Ported from Coffee CLI's ContributionHeatmap logic; no React, no IPC, so
// it's unit-testable and shared between the grid and the stat cards.

import type { HeatmapEntry } from '../../api/aiCareer';

// 30 weeks ≈ 7 months — matches the backend's 210-day lookback so the grid
// spans the full data range (and the full row width).
export const WEEKS = 30;
export const DAYS = 7;

export type HeatLevel = 0 | 1 | 2 | 3 | 4;

export interface DayCell {
  date: string; // YYYY-MM-DD in local time
  count: number;
  level: HeatLevel;
  future: boolean;
}

export interface DayBuckets {
  messages: Map<string, number>;
  sessions: Map<string, number>;
}

export interface CareerStats {
  totalSessions: number;
  totalMessages: number;
  activeDays: number;
  currentStreak: number;
  longestStreak: number;
}

export function localDayKey(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

// Square-root scaling so a single marathon day doesn't squash the rest of the
// grid into level 1 — perceptual ramp, like GitHub's.
export function levelFor(count: number, max: number): HeatLevel {
  if (count <= 0) return 0;
  if (max <= 1) return 1;
  const ratio = Math.sqrt(count) / Math.sqrt(max);
  if (ratio > 0.75) return 4;
  if (ratio > 0.5) return 3;
  if (ratio > 0.25) return 2;
  return 1;
}

// Aggregate raw entries into per-day (messages, sessions) maps. A day's key
// exists in `sessions` only if it had at least one session.
export function entriesToBuckets(entries: HeatmapEntry[]): DayBuckets {
  const messages = new Map<string, number>();
  const sessions = new Map<string, number>();
  for (const e of entries) {
    const key = localDayKey(new Date(e.ts * 1000));
    messages.set(key, (messages.get(key) ?? 0) + e.count);
    sessions.set(key, (sessions.get(key) ?? 0) + 1);
  }
  return { messages, sessions };
}

// Build the WEEKS×DAYS grid ending on the current week, with per-cell level.
export function buildGrid(buckets: DayBuckets, now = new Date()): DayCell[][] {
  const today = new Date(now);
  today.setHours(0, 0, 0, 0);
  const endDate = new Date(today);
  endDate.setDate(endDate.getDate() + (6 - today.getDay()));

  const grid: DayCell[][] = [];
  let max = 0;
  for (let col = 0; col < WEEKS; col++) {
    const week: DayCell[] = [];
    for (let row = 0; row < DAYS; row++) {
      const d = new Date(endDate);
      d.setDate(d.getDate() - ((WEEKS - 1 - col) * 7 + (DAYS - 1 - row)));
      const key = localDayKey(d);
      const count = buckets.messages.get(key) ?? 0;
      if (count > max) max = count;
      week.push({ date: key, count, level: 0, future: d.getTime() > today.getTime() });
    }
    grid.push(week);
  }
  for (const week of grid) {
    for (const cell of week) {
      cell.level = cell.future ? 0 : levelFor(cell.count, max);
    }
  }
  return grid;
}

// Compact number formatting for the stat cards:
//   271 → "271", 123635 → "123.6K", 951000000 → "951M".
export function formatCompact(n: number): string {
  if (n < 1000) return n.toLocaleString();
  if (n < 1_000_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}K`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
  return `${(n / 1_000_000_000).toFixed(1).replace(/\.0$/, '')}B`;
}

function parseDayKey(key: string): Date {
  const [y, m, d] = key.split('-').map(Number);
  return new Date(y, m - 1, d);
}

function isDayBefore(earlier: Date, later: Date): boolean {
  const next = new Date(earlier);
  next.setDate(next.getDate() + 1);
  return localDayKey(next) === localDayKey(later);
}

// Five summary stats derived from the per-day buckets.
export function deriveStats(buckets: DayBuckets): CareerStats {
  let totalSessions = 0;
  for (const v of buckets.sessions.values()) totalSessions += v;
  let totalMessages = 0;
  for (const v of buckets.messages.values()) totalMessages += v;
  const activeDays = buckets.sessions.size;

  // Current streak: consecutive active days ending today (or yesterday, so an
  // as-yet-inactive today doesn't zero out a live streak).
  let currentStreak = 0;
  const cursor = new Date();
  cursor.setHours(0, 0, 0, 0);
  if (!buckets.sessions.has(localDayKey(cursor))) {
    cursor.setDate(cursor.getDate() - 1);
  }
  while (buckets.sessions.has(localDayKey(cursor))) {
    currentStreak++;
    cursor.setDate(cursor.getDate() - 1);
  }

  // Longest streak: longest run of consecutive calendar days with activity.
  let longestStreak = 0;
  let run = 0;
  let prev: Date | null = null;
  for (const key of [...buckets.sessions.keys()].sort()) {
    const cur = parseDayKey(key);
    run = prev && isDayBefore(prev, cur) ? run + 1 : 1;
    if (run > longestStreak) longestStreak = run;
    prev = cur;
  }

  return { totalSessions, totalMessages, activeDays, currentStreak, longestStreak };
}
