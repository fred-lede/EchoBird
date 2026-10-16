// AI news: original list, source toggle and date archive.
import {
  createContext,
  useContext,
  useState,
  useEffect,
  useCallback,
  useMemo,
  useRef,
} from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { ChevronDown, ChevronRight, ExternalLink, RefreshCw } from 'lucide-react';
import { useI18n } from '../../hooks/useI18n';
import { ViewModeTabs } from '../../components/ViewModeTabs';
import { fetchFeed, filterNewsItems, type NewsItem } from './feed';
import type { TKey } from '../../i18n';

// ===== Archive (disk-backed) =====
// History lives in ~/.echobird/pulse/YYYY/MM/DD_{lang}.json, fanned out
// by the Rust pulse_archive service. Previous versions cached items in
// localStorage which was capped at 3000 entries AND wiped on every Tauri
// WebView origin reset — that's why users kept seeing "only the last
// few days" after each app upgrade. The disk archive removes both
// problems: there is no cap, and the data is independent of the WebView.
//
// The 30-min cadence and the legacy `lastFetched` timestamp still live
// in localStorage because they're (a) tiny and (b) safe to lose — if a
// WebView reset wipes them we simply refetch once.

const FEED_META = (lang: 'zh' | 'en') => `pulse:meta:${lang}`;
const REFRESH_AFTER_MS = 30 * 60 * 1000;

// Legacy cache key. Items kept under this key before the disk archive
// landed are migrated to disk on first run and then the key is removed.
const LEGACY_ITEMS_KEY = (lang: 'zh' | 'en') => `pulse:items:${lang}`;
const MIGRATION_DONE_KEY = (lang: 'zh' | 'en') => `pulse:migrated-to-disk:${lang}`;

interface FeedMeta {
  lastFetched: number;
}

const loadItems = (lang: 'zh' | 'en'): Promise<NewsItem[]> =>
  invoke<NewsItem[]>('pulse_load_all', { lang }).then(filterNewsItems);
const saveItems = async (lang: 'zh' | 'en', items: NewsItem[]): Promise<void> => {
  const allowed = filterNewsItems(items);
  if (allowed.length > 0) await invoke('pulse_save', { lang, items: allowed });
};
const loadMeta = (lang: 'zh' | 'en'): FeedMeta | null => {
  try {
    const raw = localStorage.getItem(FEED_META(lang));
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
};
const saveMeta = (lang: 'zh' | 'en', m: FeedMeta) => {
  try {
    localStorage.setItem(FEED_META(lang), JSON.stringify(m));
  } catch {
    /* quota */
  }
};

// One-shot move from the old localStorage cache into the disk archive.
// Runs at most once per (lang, install). Failure leaves the legacy key
// intact and the migration flag unset, so we'll retry on next launch.
const migrateLegacyToDisk = async (lang: 'zh' | 'en'): Promise<void> => {
  if (localStorage.getItem(MIGRATION_DONE_KEY(lang)) === '1') return;
  try {
    const raw = localStorage.getItem(LEGACY_ITEMS_KEY(lang));
    if (raw) {
      const items: NewsItem[] = JSON.parse(raw);
      await saveItems(lang, items);
    }
    localStorage.removeItem(LEGACY_ITEMS_KEY(lang));
    localStorage.setItem(MIGRATION_DONE_KEY(lang), '1');
  } catch (e) {
    console.warn(`[pulse] migrate ${lang} failed, will retry next launch`, e);
  }
};

// ===== Helpers =====

const openExternal = (url: string) => shellOpen(url).catch(() => window.open(url, '_blank'));

const formatRelative = (ts: number, t: (key: TKey) => string): string => {
  const sec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (sec < 60) return t('pulse.relJustNow');
  if (sec < 3600) return t('pulse.relMinutes').replace('{n}', String(Math.floor(sec / 60)));
  if (sec < 86400) return t('pulse.relHours').replace('{n}', String(Math.floor(sec / 3600)));
  return t('pulse.relDays').replace('{n}', String(Math.floor(sec / 86400)));
};

// URL-path-driven classification so that *.blog* hosts and lab-name sources
// stay in News. e.g. github.blog → news (engineering articles), but
// github.com/owner/repo → project; huggingface.co/blog → news, but
// huggingface.co/spaces|models|datasets → project. The source-name regex is
// kept only for upstream ZH aggregators that label items with explicit
// project markers like "GitHub Trending" or "开源周报".
const isProjectItem = (item: NewsItem): boolean => {
  const url = item.url || '';
  if (/^https?:\/\/(www\.)?github\.com\//i.test(url)) return true;
  if (/^https?:\/\/huggingface\.co\/(spaces|models|datasets)\//i.test(url)) return true;
  const s = `${item.source} ${item.site_name || ''}`.toLowerCase();
  return /trending|开源/i.test(s);
};

// Many items have null published_at; first_seen_at is always present.
// Some Chinese aggregators label local CST as "Z" (UTC), putting published_at
// up to 8h in the future — fall back to first_seen_at in that case so the
// relative-time display doesn't pin every fresh item at "刚刚".
const itemTs = (item: NewsItem): string => {
  const pub = item.published_at;
  const seen = item.first_seen_at || item.last_seen_at || '';
  if (!pub) return seen;
  const pubMs = Date.parse(pub);
  if (Number.isFinite(pubMs) && pubMs > Date.now() + 5 * 60 * 1000) return seen || pub;
  return pub;
};

// Local-timezone YYYY-MM-DD. A naive `ts.slice(0, 10)` slices the raw ISO
// string and so groups items by UTC date — for CST users that pushes
// every item from CST 00:00–08:00 back into yesterday's archive bucket.
const itemLocalDate = (item: NewsItem): string => {
  const ts = itemTs(item);
  if (!ts) return '';
  const ms = Date.parse(ts);
  if (!Number.isFinite(ms)) return ts.slice(0, 10);
  const d = new Date(ms);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
};

// Language detection by CJK presence in the title. Bilingual side effect:
// Chinese-app users see WeChat 公众号 too (and have WeChat installed to read them);
// English-app users see only Latin-script items, so the WeChat awkwardness is gone.
const CJK_RE = /[㐀-䶿一-鿿豈-﫿]/;
const itemLang = (item: NewsItem): 'zh' | 'en' => {
  const probe = item.title || item.title_zh || '';
  return CJK_RE.test(probe) ? 'zh' : 'en';
};

// Upstream wraps western sources with Chinese decorations like "Lobsters (技术社区)"
// or "Hacker News · 24h最热". Strip those for EN display so the feed reads as pure global content.
const cleanSourceForEn = (source: string): string => {
  if (!source) return source;
  return (
    source
      // Drop parenthetical groups that contain CJK: "(技术社区)", "(创意工作者社区)"
      .replace(/[（(][^)）]*[一-鿿][^)）]*[)）]/g, '')
      // Drop trailing "· ..." segments containing CJK: "Hacker News · 24h最热"
      .replace(/[·∙•|]\s*[^·•|]*[一-鿿][^·•|]*$/g, '')
      .replace(/\s+/g, ' ')
      .trim()
  );
};

// ===== Context =====

interface AiPulseContextValue {
  items: NewsItem[];
  initialLoading: boolean;
  syncing: boolean;
  error: string | null;
  // Currently viewed date (YYYY-MM-DD). null until set — AiPulsePanel
  // auto-fills it with the latest cached date on first mount, after
  // which date-button clicks just replace the value.
  selectedDate: string | null;
  selectDate: (date: string) => void;
  retry: () => void;
  // Source of items to display. For zh locale, the user toggles
  // between 'zh' (国内 — SuYxh aggregator) and 'en' (全球 — our
  // HN/RSS-built feed). For en locale, always 'en' — see comment in
  // the Provider for the rationale (no need to expose Chinese-titled
  // items to global users).
  feedSource: 'zh' | 'en';
  setFeedSource: (s: 'zh' | 'en') => void;
}

const AiPulseContext = createContext<AiPulseContextValue | null>(null);

function useAiPulse() {
  const ctx = useContext(AiPulseContext);
  if (!ctx) throw new Error('AiPulse context missing');
  return ctx;
}

// ===== Provider =====

// localStorage key for the zh-user feed-source toggle (国内 / 全球). en
// users don't get the toggle so this key is only ever written by zh
// sessions.
const FEED_SOURCE_KEY = 'pulse:feed-source';

export function AiPulseProvider({
  children,
  active,
}: {
  children: React.ReactNode;
  active: boolean;
}) {
  const { locale } = useI18n();
  // Content routing intentionally narrower than UI routing: only zh-Hans
  // (mainland Simplified) gets the CN-aggregated feed + 国内/全球 toggle.
  // zh-Hant (TW/HK/MO) and ja users see the EN feed — TW/HK devs follow
  // the international AI stack (NVIDIA/CUDA + arXiv + HN), and the SuYxh
  // feed is dominated by CN-domestic platforms (Bilibili, 知乎, 飞桨).
  const isZhLocale = locale === 'zh-Hans';

  // zh users get a 国内/全球 toggle; their selection is persisted in
  // localStorage and lives on `zhFeedPreference`. en users always see
  // 'en' regardless of what zh-side preference was stashed earlier —
  // they don't get the toggle and shouldn't be exposed to Chinese-
  // titled content.
  //
  // feedSource is DERIVED from (isZhLocale, zhFeedPreference) rather
  // than a separate stateful field — so when the user flips the app
  // locale in Settings, feedSource recomputes immediately and the
  // News page swaps to the appropriate feed (previous bug:
  // feedSource was useState'd from localStorage once at mount and
  // never re-synced with locale, leaving zh content visible after
  // switching to en).
  const [zhFeedPreference, setZhFeedPreferenceState] = useState<'zh' | 'en'>(() => {
    const stored = localStorage.getItem(FEED_SOURCE_KEY);
    return stored === 'en' ? 'en' : 'zh';
  });
  const feedSource: 'zh' | 'en' = isZhLocale ? zhFeedPreference : 'en';
  const setFeedSource = useCallback(
    (s: 'zh' | 'en') => {
      if (!isZhLocale) return; // en users have no toggle, no-op
      localStorage.setItem(FEED_SOURCE_KEY, s);
      setZhFeedPreferenceState(s);
    },
    [isZhLocale]
  );

  // Active language for fetching/caching. Derived from feedSource so
  // that flipping the toggle OR the locale re-runs the cache+fetch
  // effect below.
  const lang: 'zh' | 'en' = feedSource;

  const [items, setItems] = useState<NewsItem[]>([]);
  const [initialLoading, setInitialLoading] = useState(true);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedDate, setSelectedDate] = useState<string | null>(null);
  const seq = useRef(0);
  const requestRef = useRef<AbortController | null>(null);
  const loadedLang = useRef(lang);

  const selectDate = useCallback((date: string) => setSelectedDate(date), []);

  const sync = useCallback(async (targetLang: 'zh' | 'en', force = false) => {
    if (requestRef.current) return;
    const controller = new AbortController();
    requestRef.current = controller;
    const my = ++seq.current;
    const current = () => my === seq.current && !controller.signal.aborted;
    setSyncing(true);
    setError(null);
    try {
      await migrateLegacyToDisk(targetLang);
      if (!current()) return;
      const cached = await loadItems(targetLang);
      if (!current()) return;
      setItems(cached);
      if (cached.length > 0) setInitialLoading(false);

      const meta = loadMeta(targetLang);
      if (!force && meta && Date.now() - meta.lastFetched < REFRESH_AFTER_MS && cached.length > 0) {
        return;
      }
      const feed = await fetchFeed(targetLang, controller.signal);
      if (!current()) return;
      await saveItems(targetLang, feed.items);
      if (!current()) return;
      const merged = await loadItems(targetLang);
      if (!current()) return;
      const now = Date.now();
      saveMeta(targetLang, { lastFetched: now });
      setItems(merged);
    } catch (e) {
      if (current()) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (current()) {
        requestRef.current = null;
        setInitialLoading(false);
        setSyncing(false);
      }
    }
  }, []);

  const retry = useCallback(() => {
    if (active) void sync(lang, true);
  }, [active, sync, lang]);

  useEffect(() => {
    if (!active) return;
    if (loadedLang.current !== lang) {
      loadedLang.current = lang;
      setItems([]);
      setSelectedDate(null);
      setInitialLoading(true);
    }
    // Navigation starts archive hydration and its pending indicator together.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void sync(lang);
    return () => {
      seq.current += 1;
      requestRef.current?.abort();
      requestRef.current = null;
    };
  }, [active, lang, sync]);

  const value = useMemo<AiPulseContextValue>(
    () => ({
      items,
      initialLoading,
      syncing,
      error,
      selectedDate,
      selectDate,
      retry,
      feedSource,
      setFeedSource,
    }),
    [
      items,
      initialLoading,
      syncing,
      error,
      selectedDate,
      selectDate,
      retry,
      feedSource,
      setFeedSource,
    ]
  );

  return <AiPulseContext.Provider value={value}>{children}</AiPulseContext.Provider>;
}

// ===== Title actions =====

export function AiPulseTitleActions() {
  const { t, locale } = useI18n();
  const { syncing, retry, feedSource, setFeedSource } = useAiPulse();
  // Toggle only meaningful for the zh-Hans CN feed; other locales pinned to EN.
  const isZhLocale = locale === 'zh-Hans';
  return (
    <div className="ml-auto flex-shrink-0 flex items-center gap-2">
      {/* 国内/全球 toggle — only rendered for zh users. Labels are
          hardcoded Chinese because en users never see this control
          (they're pinned to the en feed by the Provider). Avoids
          adding two i18n keys that would never be translated. */}
      {isZhLocale && (
        <ViewModeTabs
          value={feedSource}
          onChange={setFeedSource}
          options={[
            { value: 'zh', label: '国内' },
            { value: 'en', label: '全球' },
          ]}
        />
      )}
      <button
        onClick={retry}
        disabled={syncing}
        className={`text-sm px-3 py-1.5 border rounded-md transition-colors flex items-center gap-2 ${
          !syncing
            ? 'border-cyber-border/50 text-cyber-text hover:bg-cyber-text/10'
            : 'border-cyber-border text-cyber-text-muted'
        }`}
      >
        <RefreshCw size={13} className={syncing ? 'animate-spin' : ''} />
        {t('btn.refresh')}
      </button>
    </div>
  );
}

// ===== Item row =====

function ItemRow({ item }: { item: NewsItem }) {
  const { t } = useI18n();
  const { feedSource } = useAiPulse();
  // Title language follows the feed source, NOT the UI locale — a zh
  // user viewing the 全球 feed should see English titles (those are
  // what's actually in `item.title` for the en feed). Falling back
  // through locale would surface raw `title` (Chinese) for en items.
  const lang = feedSource;
  const tsRaw = itemTs(item);
  const ts = tsRaw ? Date.parse(tsRaw) : 0;
  const title = lang === 'en' ? item.title_en || item.title : item.title_zh || item.title;
  const sourceLabel = lang === 'en' ? cleanSourceForEn(item.source) : item.source;

  return (
    <button
      onClick={() => openExternal(item.url)}
      className="group w-full text-left rounded-card hover:bg-cyber-text/5 transition-colors px-5 py-4 flex gap-3 items-start"
    >
      <div className="flex-1 min-w-0">
        <div className="text-[15px] font-medium text-cyber-text leading-relaxed group-hover:text-cyber-accent transition-colors">
          {title}
        </div>
        <div className="mt-2 text-[13px] text-cyber-text-secondary flex items-center gap-2">
          <span className="truncate max-w-[240px]">{sourceLabel}</span>
          {ts > 0 && (
            <>
              <span className="opacity-50">·</span>
              <span>{formatRelative(ts, t)}</span>
            </>
          )}
        </div>
      </div>
      <ExternalLink
        size={14}
        className="flex-shrink-0 mt-1 text-cyber-text-muted/50 group-hover:text-cyber-text transition-colors"
      />
    </button>
  );
}

// ===== Feed =====

function ItemFeed() {
  const { t } = useI18n();
  const { items, initialLoading, syncing, error, selectedDate, retry, feedSource } = useAiPulse();
  // Filter on feedSource (not UI locale) so the 国内/全球 toggle
  // actually swaps the visible items. en locale always pins feedSource
  // to 'en' (see Provider), so this is identical to the old behaviour
  // for en users.
  const lang: 'zh' | 'en' = feedSource;

  // Keep the old news-only partition; project links are excluded.
  const newsItems = useMemo(() => {
    const langMatched = items.filter((it) => itemLang(it) === lang);
    return langMatched.filter((it) => !isProjectItem(it));
  }, [items, lang]);

  // Latest cached date for this language. Used as the fallback when
  // selectedDate is null (panel hasn't initialised it yet, or items just
  // arrived). Computing here — not in the provider — keeps the default
  // language-aware: zh users won't accidentally land on an en-only date.
  const latestDate = useMemo(() => {
    let latest = '';
    for (const it of newsItems) {
      const d = itemLocalDate(it);
      if (d && d > latest) latest = d;
    }
    return latest;
  }, [newsItems]);

  const effectiveDate = selectedDate || latestDate;

  // Per-day batching: feed shows a single day's items. Switching dates in
  // the right panel replaces the batch wholesale instead of scrolling
  // through every older day, which was the original UX bug.
  const visible = useMemo(() => {
    if (!effectiveDate) return [];
    return newsItems.filter((it) => itemLocalDate(it) === effectiveDate);
  }, [newsItems, effectiveDate]);

  if ((initialLoading || syncing) && visible.length === 0 && newsItems.length === 0) {
    return (
      <div className="space-y-2">
        <div aria-hidden="true" className="h-0.5" />
        {Array.from({ length: 8 }).map((_, i) => (
          <div key={i} className="p-3 bg-cyber-surface rounded-card animate-pulse">
            <div className="h-3 w-full bg-cyber-border/50 rounded mb-2" />
            <div className="h-3 w-2/3 bg-cyber-border/30 rounded" />
          </div>
        ))}
      </div>
    );
  }

  if (error && newsItems.length === 0) {
    return (
      <div className="p-8 text-center text-sm font-mono">
        <div className="text-cyber-warning mb-2">{t('pulse.fetchFailed')}</div>
        <div className="text-xs text-cyber-text-muted/60 mb-4 break-all max-w-md mx-auto">
          {error}
        </div>
        <button
          onClick={retry}
          className="text-xs px-4 py-2 border border-cyber-border/50 rounded text-cyber-text hover:bg-cyber-text/10 transition-colors"
        >
          {t('btn.refresh')}
        </button>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {error && (
        <div role="alert" className="px-5 py-2 text-sm text-cyber-warning">
          {t('pulse.fetchFailed')}
        </div>
      )}
      <div aria-hidden="true" className="h-0.5" />
      {visible.length === 0 ? (
        <div className="p-8 text-center text-sm text-cyber-text-secondary font-mono">
          {t('pulse.empty')}
        </div>
      ) : (
        visible.map((item) => <ItemRow key={item.id} item={item} />)
      )}
    </div>
  );
}

export function AiNewsMain() {
  return (
    <div className="flex-1 overflow-y-auto pb-4 page-scroll">
      <ItemFeed />
    </div>
  );
}

// ===== Right panel: date tree =====

function groupByMonth(dates: string[]): Map<string, string[]> {
  const map = new Map<string, string[]>();
  for (const d of dates) {
    const ym = d.slice(0, 7);
    if (!map.has(ym)) map.set(ym, []);
    map.get(ym)!.push(d);
  }
  // Fill in-month gaps with placeholder days so the sidebar shows "0"
  // for a day in the middle of the user's archive that has no items —
  // the upstream is a 7-day sliding window, so a missed day can never
  // be backfilled, and the "0" tells the user we actually checked
  // rather than the day simply disappearing. We never extend beyond
  // the first or last archived day of a month, because that would
  // clutter the list with strings of days the user never reached.
  for (const [ym, days] of map) {
    if (days.length < 2) continue;
    const nums = days.map((d) => parseInt(d.slice(8, 10), 10));
    let min = nums[0];
    let max = nums[0];
    for (const n of nums) {
      if (n < min) min = n;
      if (n > max) max = n;
    }
    if (max - min + 1 === days.length) continue; // already dense
    const filled: string[] = [];
    for (let day = max; day >= min; day--) {
      filled.push(`${ym}-${String(day).padStart(2, '0')}`);
    }
    map.set(ym, filled);
  }
  return map;
}

export function AiPulsePanel() {
  const { t } = useI18n();
  const { items, selectedDate, selectDate, feedSource, initialLoading, syncing } = useAiPulse();
  // Date-button counts must mirror ItemFeed's filter (feedSource, not
  // locale) so the number on each date button reflects what the user
  // will actually see when they click.
  const lang: 'zh' | 'en' = feedSource;

  // Per-day item count, filtered by language so the number on
  // each date button reflects what the user will actually see in the
  // feed when they click. cachedDates derives from the same map's keys
  // — a date with zero matching items wouldn't appear at all.
  // Match the news list by excluding project links.
  const dateCount = useMemo(() => {
    const map = new Map<string, number>();
    for (const it of items) {
      if (itemLang(it) !== lang) continue;
      const proj = isProjectItem(it);
      if (proj) continue;
      const d = itemLocalDate(it);
      if (!d) continue;
      map.set(d, (map.get(d) || 0) + 1);
    }
    return map;
  }, [items, lang]);

  const cachedDates = useMemo(
    () => Array.from(dateCount.keys()).sort((a, b) => b.localeCompare(a)),
    [dateCount]
  );

  const grouped = useMemo(() => groupByMonth(cachedDates), [cachedDates]);
  const months = useMemo(() => Array.from(grouped.keys()), [grouped]);

  // Auto-pick the latest date once items arrive so the feed has something
  // to show without forcing the user to click first. Re-runs only when
  // selectedDate is still null — once the user picks anything, we leave
  // their choice alone even if a refresh adds a newer day.
  useEffect(() => {
    if (selectedDate) return;
    if (cachedDates.length === 0) return;
    selectDate(cachedDates[0]);
  }, [cachedDates, selectedDate, selectDate]);

  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setExpanded((prev) =>
      prev.size === 0 && months.length > 0 ? new Set(months.slice(0, 1)) : prev
    );
  }, [months]);

  const toggle = (ym: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(ym)) {
        next.delete(ym);
      } else {
        next.add(ym);
      }
      return next;
    });

  return (
    <>
      <div className="px-3 py-2 mb-1 bg-transparent">
        <div className="text-[15px] font-semibold text-cyber-text">{t('pulse.archive')}</div>
      </div>
      <div className="flex-1 px-2 overflow-y-auto pb-4 page-scroll">
        {cachedDates.length === 0 ? (
          <div className="px-3 py-8 text-center text-[14px] text-cyber-text-muted leading-relaxed">
            {t(initialLoading || syncing ? 'pulse.loadingFirst' : 'pulse.empty')}
          </div>
        ) : (
          <div className="space-y-1">
            {months.map((ym) => {
              const isOpen = expanded.has(ym);
              const days = grouped.get(ym) || [];
              // Sum items across the month so the badge mirrors what
              // the user gets when they expand the group, not a
              // redundant day count (the day chevrons already
              // implicitly convey day count by their list length).
              const monthCount = days.reduce((s, d) => s + (dateCount.get(d) || 0), 0);
              return (
                <div key={ym}>
                  <button
                    onClick={() => toggle(ym)}
                    className="w-full flex items-center gap-1.5 px-2 py-2 text-[15px] font-mono text-cyber-text-secondary hover:text-cyber-text rounded transition-colors"
                  >
                    {isOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                    <span>{ym}</span>
                    <span className="ml-auto text-[13px] text-cyber-text-muted">{monthCount}</span>
                  </button>
                  {isOpen && (
                    <div className="ml-3 border-l border-cyber-border/20 pl-2 space-y-0.5">
                      {days.map((d) => {
                        const isActive = d === selectedDate;
                        const count = dateCount.get(d) || 0;
                        return (
                          <button
                            key={d}
                            onClick={() => selectDate(d)}
                            className={`w-full flex items-center px-2 py-1.5 rounded text-[14px] font-mono transition-colors ${
                              isActive
                                ? 'bg-cyber-accent/15 text-cyber-text border border-cyber-accent/40'
                                : 'text-cyber-text-secondary hover:bg-cyber-elevated hover:text-cyber-text border border-transparent'
                            }`}
                          >
                            <span>{d.slice(8)}</span>
                            <span
                              className={`ml-auto text-[12px] ${isActive ? 'text-cyber-text-secondary' : 'text-cyber-text-muted'}`}
                            >
                              {count}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </>
  );
}
