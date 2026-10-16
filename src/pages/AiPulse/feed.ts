// ===== Mirror config =====

// ZH feed: SuYxh's bilingual (mostly CN) aggregator — four mirrors.
const PULSE_MIRRORS_ZH: { name: string; base: string }[] = [
  { name: 'echobird', base: 'https://echobird.ai/pulse' },
  // Upstream's own GitHub Pages — official publish point, freshest data
  // (updates within minutes of the upstream Action), CORS enabled,
  // and *.github.io is generally GFW-routable from mainland China.
  { name: 'pages', base: 'https://suyxh.github.io/ai-news-aggregator/data' },
  // jsDelivr CDN — global CDN that proxies the repo. Caches @main for
  // up to 12h so it lags the upstream; useful when github.io is flaky.
  { name: 'jsdelivr', base: 'https://cdn.jsdelivr.net/gh/SuYxh/ai-news-aggregator@main/data' },
  // Last-resort: raw.githubusercontent.com — works globally except in
  // mainland China (GFW-blocked).
  {
    name: 'github-raw',
    base: 'https://raw.githubusercontent.com/SuYxh/ai-news-aggregator/main/data',
  },
];

// EN feed: built by scripts/build_en_pulse.py and committed to docs/pulse/.
// Only available from our infrastructure — none of SuYxh's mirrors carry it.
// Order trades latency vs. CN-reachability the same way as the ZH chain.
const PULSE_MIRRORS_EN: { name: string; base: string }[] = [
  { name: 'echobird', base: 'https://echobird.ai/pulse' },
  // jsDelivr proxy of OUR repo's docs/pulse — useful when echobird.ai is throttled.
  { name: 'jsdelivr', base: 'https://cdn.jsdelivr.net/gh/edison7009/EchoBird@main/docs/pulse' },
  {
    name: 'github-raw',
    base: 'https://raw.githubusercontent.com/edison7009/EchoBird/main/docs/pulse',
  },
];

// Fetch a seven-day window; successful refreshes merge into the local archive.
const FEED_FILE_ZH = 'latest-7d.json';
const FEED_FILE_EN = 'latest-7d-en.json';

// ===== Types =====

export interface RawFeed {
  generated_at: string;
  window_hours: number;
  total_items: number;
  items: NewsItem[];
}

export interface NewsItem {
  id: string;
  site_id?: string;
  site_name?: string;
  source: string;
  title: string;
  url: string;
  published_at: string | null;
  first_seen_at?: string;
  last_seen_at?: string;
  title_zh?: string | null;
  title_en?: string | null;
}

// Match scripts/filter_pulse.py for mirrors and archives that bypass our workflow.
const BLOCKED_HOSTS = ['x.com', 'twitter.com', 'v2ex.com'];
const BLOCKED_TITLE_RE =
  /社区公告|[【[](?:广告|推广|赞助|AD|PR|Sponsored)[】\]]|^(?:广告|推广|赞助)[:：\s|]/i;

export function filterNewsItems(items: NewsItem[]): NewsItem[] {
  return items.filter((item) => {
    let host: string;
    try {
      host = new URL(item.url).hostname.toLowerCase().replace(/\.+$/, '');
    } catch {
      return false;
    }
    if (BLOCKED_HOSTS.some((blocked) => host === blocked || host.endsWith(`.${blocked}`))) {
      return false;
    }
    return ![item.title, item.title_zh, item.title_en].some((title) =>
      BLOCKED_TITLE_RE.test(title || '')
    );
  });
}

// ===== Network: mirror-aware fetch =====

// Sticky preferred-mirror index, separate per feed: once a mirror serves a
// good response it stays at the head of the chain for subsequent fetches.
const preferredMirror: Record<'zh' | 'en', number> = { zh: 0, en: 0 };

const looksLikeHtml = (s: string): boolean => {
  const head = s.slice(0, 200).trimStart().toLowerCase();
  return head.startsWith('<!doctype html') || head.startsWith('<html');
};

async function fetchOneFeed(lang: 'zh' | 'en', signal: AbortSignal): Promise<RawFeed> {
  const mirrors = lang === 'en' ? PULSE_MIRRORS_EN : PULSE_MIRRORS_ZH;
  const file = lang === 'en' ? FEED_FILE_EN : FEED_FILE_ZH;
  const start = preferredMirror[lang];
  const order = [...mirrors.slice(start), ...mirrors.slice(0, start)];
  let lastErr: unknown = null;
  for (let i = 0; i < order.length; i++) {
    signal.throwIfAborted();
    const mirror = order[i];
    // Bound each mirror: a throttled (stalled, not refused) connection would
    // otherwise leave this fetch pending forever and hang the whole fallback
    // chain. Abort after 10s so we advance to the next mirror.
    const controller = new AbortController();
    const abort = () => controller.abort();
    signal.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(abort, 10000);
    try {
      const res = await fetch(`${mirror.base}/${file}`, {
        cache: 'no-cache',
        signal: controller.signal,
      });
      if (!res.ok) {
        lastErr = new Error(`${mirror.name} ${res.status}`);
        continue;
      }
      const text = await res.text();
      if (looksLikeHtml(text)) {
        lastErr = new Error(`${mirror.name} returned HTML`);
        continue;
      }
      try {
        const parsed = JSON.parse(text);
        if (
          !Array.isArray(parsed.items) ||
          parsed.items.length === 0 ||
          !parsed.items.every(
            (item: NewsItem) =>
              typeof item.id === 'string' &&
              typeof item.title === 'string' &&
              typeof item.source === 'string' &&
              typeof item.url === 'string' &&
              /^https?:\/\//i.test(item.url)
          )
        ) {
          lastErr = new Error(`${mirror.name} invalid feed`);
          continue;
        }
        parsed.items = filterNewsItems(parsed.items);
        parsed.total_items = parsed.items.length;
        preferredMirror[lang] = (start + i) % mirrors.length;
        return parsed;
      } catch {
        lastErr = new Error(`${mirror.name} bad JSON`);
        continue;
      }
    } catch (e) {
      lastErr = e;
    } finally {
      clearTimeout(timer);
      signal.removeEventListener('abort', abort);
    }
  }
  throw lastErr || new Error('all mirrors failed');
}

// Soft-fallback wrapper: en users hit our self-built file first, but if that
// file isn't yet deployed (fresh PR, GHA hasn't run, etc.) fall through to
// the ZH file — its EN-titled subset (~2000 items) is the same content the
// app shipped before this feature, so the page never looks empty.
export async function fetchFeed(lang: 'zh' | 'en', signal: AbortSignal): Promise<RawFeed> {
  if (lang === 'en') {
    try {
      return await fetchOneFeed('en', signal);
    } catch (e) {
      signal.throwIfAborted();
      console.warn('[pulse] EN feed unavailable, falling back to bilingual feed:', e);
      return await fetchOneFeed('zh', signal);
    }
  }
  return fetchOneFeed('zh', signal);
}
