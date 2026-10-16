import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const payload = {
  items: [{ id: 'article', title: 'AI news', source: 'AI Lab', url: 'https://example.com/news' }],
  total_items: 1,
};
const response = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });
let fetchFeed: typeof import('./feed').fetchFeed;
let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(async () => {
  vi.resetModules();
  ({ fetchFeed } = await import('./feed'));
  fetchMock = vi.fn();
  vi.stubGlobal('fetch', fetchMock);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('news mirrors', () => {
  it.each(['zh', 'en'] as const)(
    'filters blocked links and promotional titles from the %s fallback source',
    async (lang) => {
      const blocked = [
        { url: 'https://x.com', title: 'AI discussion' },
        { url: 'https://MOBILE.TWITTER.COM:443/user/status/1', title: 'AI discussion' },
        { url: 'https://v2ex.com?tab=ai', title: 'AI discussion' },
        { url: 'https://x.com./user', title: 'AI discussion' },
        {
          url: 'https://example.com/promo',
          title: '[推广] 一个下午验证 Claude 中转，送 10 美元试用金',
        },
        { url: 'https://example.com/ad', title: '【广告】AI 产品' },
        { url: 'https://example.com/sponsor', title: '[Sponsored] AI trial credits' },
        {
          url: 'https://example.com/translated',
          title: 'AI trial credits',
          title_zh: '[推广] AI 试用金',
        },
      ].map((item, index) => ({ ...payload.items[0], id: `blocked-${index}`, ...item }));
      const allowed = [
        payload.items[0],
        { ...payload.items[0], id: 'ad-industry', title: '广告业的 AI 转型' },
        { ...payload.items[0], id: 'sponsor-news', title: 'Anthropic 拒绝 X 公司赞助' },
        {
          ...payload.items[0],
          id: 'domain-in-title',
          title: 'AI discussion on x.com',
          url: 'https://example.com/x.com',
        },
        { ...payload.items[0], id: 'similar-domain', url: 'https://examplex.com/news' },
      ];
      fetchMock.mockResolvedValueOnce(response({}, 404)).mockResolvedValueOnce(
        response({
          items: [...blocked, ...allowed],
          total_items: blocked.length + allowed.length,
        })
      );
      const result = await fetchFeed(lang, new AbortController().signal);
      expect(fetchMock).toHaveBeenCalledTimes(2);
      expect(result.items).toEqual(allowed);
      expect(result.total_items).toBe(allowed.length);
    }
  );

  it('rejects unavailable and malformed sources, then keeps the working CDN preferred', async () => {
    fetchMock
      .mockResolvedValueOnce(response({}, 404))
      .mockResolvedValueOnce(response({ items: [{}] }))
      .mockImplementation(async () => response(payload));
    const signal = new AbortController().signal;
    expect(await fetchFeed('zh', signal)).toEqual(payload);
    expect(fetchMock.mock.calls[2][0]).toContain('cdn.jsdelivr.net/gh/SuYxh');
    await fetchFeed('zh', signal);
    expect(fetchMock.mock.calls[3][0]).toContain('cdn.jsdelivr.net/gh/SuYxh');
  });

  it('uses the bilingual fallback when the global feed is not yet deployed', async () => {
    fetchMock.mockImplementation(async (url: string) =>
      url.endsWith('latest-7d-en.json') ? response({}, 404) : response(payload)
    );
    expect(await fetchFeed('en', new AbortController().signal)).toEqual(payload);
    expect(fetchMock).toHaveBeenCalledTimes(4);
    expect(fetchMock.mock.calls[3][0]).toContain('latest-7d.json');
  });

  it('advances past a stalled mirror after ten seconds', async () => {
    vi.useFakeTimers();
    fetchMock
      .mockImplementationOnce(
        (_url, { signal }: { signal: AbortSignal }) =>
          new Promise((_resolve, reject) => {
            signal.addEventListener('abort', () => reject(new Error('timeout')));
          })
      )
      .mockImplementation(async () => response(payload));
    const pending = fetchFeed('zh', new AbortController().signal);
    await vi.advanceTimersByTimeAsync(10000);
    expect(await pending).toEqual(payload);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('cancels the request without visiting another mirror or language fallback', async () => {
    const controller = new AbortController();
    fetchMock.mockImplementation(
      (_url, { signal }: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          signal.addEventListener('abort', () => reject(new Error('cancelled')));
        })
    );
    const pending = fetchFeed('en', controller.signal);
    controller.abort();
    await expect(pending).rejects.toThrow();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
