import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ComponentProps } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CareerTooltip } from './CareerTooltip';
import { DailyActivityChart } from './DailyActivityChart';
import type { DayBuckets } from './heatmapData';
import * as api from '../../api/aiCareer';
import { useAiCareerStore } from '../../stores/aiCareerStore';
import { useNavigationStore } from '../../stores/navigationStore';
import { AiCareerMain } from './AiCareerMain';

vi.mock('../../hooks/useI18n', () => ({
  useI18n: () => ({
    locale: 'en-US',
    t: (key: string) =>
      (
        ({
          'aiCareer.messages': '{count} messages',
          'aiCareer.input': 'Input',
          'aiCareer.output': 'Output',
          'aiCareer.total': 'Total',
        }) as Record<string, string>
      )[key] ?? key,
  }),
}));
vi.mock('react-dom', () => ({ createPortal: (children: unknown) => children }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../../api/aiCareer', () => ({
  aiCareerHeatmap: vi.fn().mockResolvedValue([]),
  aiCareerTokenBytes: vi.fn().mockResolvedValue(0),
  aiCareerDailyTokens: vi.fn().mockResolvedValue({ entries: [], unavailable_families: [] }),
  getAvatar: vi.fn().mockResolvedValue(null),
  setAvatar: vi.fn(),
}));
vi.mock('./ContributionHeatmap', () => ({ ContributionHeatmap: () => null }));
vi.mock('./AiCareerPanel', () => ({ AiCareerPanel: () => null }));

describe('daily activity chart', () => {
  let renderer: ReactTestRenderer;
  let tooltipWidth = 100;
  const buckets = (messages: [string, number][] = []): DayBuckets => ({
    messages: new Map(messages),
    sessions: new Map(),
  });
  const target = {
    getBoundingClientRect: () => ({ left: 100, top: 60, width: 20 }),
  };

  type ChartProps = ComponentProps<typeof DailyActivityChart>;
  function chartProps(data: DayBuckets, overrides: Partial<ChartProps> = {}): ChartProps {
    return {
      buckets: data,
      metric: 'messages',
      onMetricChange: vi.fn(),
      tokenDays: new Map(),
      tokenStatus: 'ready',
      onTokenRetry: vi.fn(),
      ...overrides,
    };
  }

  function render(data: DayBuckets, overrides: Partial<ChartProps> = {}) {
    act(() => {
      renderer = create(<DailyActivityChart {...chartProps(data, overrides)} />, {
        createNodeMock: () => ({
          offsetWidth: tooltipWidth,
          getBoundingClientRect: () => ({ left: 100, top: 20, width: 100 }),
        }),
      });
    });
    return renderer.root.findAllByProps({ role: 'img' });
  }

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 2, 2, 12));
    tooltipWidth = 100;
    vi.stubGlobal('window', {
      innerWidth: 960,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    });
    vi.stubGlobal('document', { body: {} });
  });

  afterEach(() => {
    act(() => renderer?.unmount());
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('fills a continuous 30-day range across a month boundary and excludes outside days', () => {
    const columns = render(
      buckets([
        ['2026-02-01', 12],
        ['2026-02-15', 3],
        ['2026-03-02', 1234],
        ['2026-01-31', 99],
        ['2026-03-03', 99],
      ])
    );
    expect(columns).toHaveLength(30);
    expect(columns[0].props['aria-label']).toBe('Feb 1, 2026 · 12 messages');
    expect(columns[1].props['aria-label']).toBe('Feb 2, 2026 · 0 messages');
    expect(columns[14].props['aria-label']).toBe('Feb 15, 2026 · 3 messages');
    expect(columns[29].props['aria-label']).toBe('Mar 2, 2026 · 1,234 messages');
    expect(columns.some((column) => column.props['aria-label'].includes('99 messages'))).toBe(
      false
    );
  });

  it('removes the chart tooltip on page exit without blur or mouse movement', async () => {
    useAiCareerStore.setState(useAiCareerStore.getInitialState());
    useNavigationStore.setState({ activePage: 'aiCareer' });
    vi.clearAllMocks();
    vi.mocked(api.aiCareerDailyTokens).mockResolvedValueOnce({
      entries: [
        { ts: new Date(2026, 2, 2, 12).getTime() / 1000, input_tokens: 20, output_tokens: 5 },
      ],
      unavailable_families: [],
    });
    await act(async () => {
      renderer = create(<AiCareerMain />, {
        createNodeMock: () => ({ offsetWidth: 100 }),
      });
    });
    const columns = renderer.root.findAllByProps({ role: 'img' });
    act(() => columns[29].props.onMouseEnter({ currentTarget: target }));
    expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(1);
    await act(async () => useNavigationStore.getState().setActivePage('apps'));
    expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(0);
    expect(renderer.root.findAllByType(DailyActivityChart)).toHaveLength(0);
    vi.setSystemTime(new Date(2026, 2, 3, 12));
    vi.mocked(api.aiCareerDailyTokens).mockReturnValueOnce(new Promise(() => {}));
    await act(async () => useNavigationStore.getState().setActivePage('aiCareer'));
    expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(0);
    expect(renderer.root.findAllByType(DailyActivityChart)).toHaveLength(1);
    expect(api.aiCareerDailyTokens).toHaveBeenCalledTimes(2);
    expect(useAiCareerStore.getState().chartMetric).toBe('tokens');
    const currentColumns = renderer.root.findAllByProps({ role: 'img' });
    expect(currentColumns[28].props['aria-label']).toBe(
      'Mar 2, 2026 · Input 20 · Output 5 · Total 25'
    );
    expect(currentColumns[29].props['aria-label']).toBe(
      'Mar 3, 2026 · Input — · Output — · Total —'
    );
    expect(renderer.root.findByProps({ role: 'tabpanel' }).props['aria-busy']).toBe(false);
  });

  it('includes leap day in its calendar range', () => {
    vi.setSystemTime(new Date(2028, 2, 2, 12));
    const columns = render(buckets([['2028-02-29', 7]]));
    expect(columns[0].props['aria-label']).toBe('Feb 2, 2028 · 0 messages');
    expect(columns[27].props['aria-label']).toBe('Feb 29, 2028 · 7 messages');
    expect(columns[29].props['aria-label']).toBe('Mar 2, 2028 · 0 messages');
  });

  it('advances the local calendar on a post-midnight render even when buckets keep their identity', () => {
    vi.setSystemTime(new Date(2026, 2, 2, 23, 59));
    const data = buckets([
      ['2026-03-02', 4],
      ['2026-03-03', 7],
    ]);
    render(data);
    expect(renderer.root.findAllByProps({ role: 'img' })[29].props['aria-label']).toBe(
      'Mar 2, 2026 · 4 messages'
    );
    const reads = vi.mocked(api.aiCareerDailyTokens).mock.calls.length;
    vi.setSystemTime(new Date(2026, 2, 3, 0, 1));
    act(() =>
      renderer.update(
        <DailyActivityChart
          {...chartProps(data, {
            metric: 'tokens',
            tokenDays: new Map([['2026-03-03', { input: 20, output: 5 }]]),
          })}
        />
      )
    );
    const columns = renderer.root.findAllByProps({ role: 'img' });
    expect(columns[0].props['aria-label']).toContain('Feb 2, 2026');
    expect(columns[29].props['aria-label']).toBe('Mar 3, 2026 · Input 20 · Output 5 · Total 25');
    act(() => renderer.update(<DailyActivityChart {...chartProps(data)} />));
    expect(renderer.root.findAllByProps({ role: 'img' })[29].props['aria-label']).toBe(
      'Mar 3, 2026 · 7 messages'
    );
    expect(api.aiCareerDailyTokens).toHaveBeenCalledTimes(reads);
  });

  it('keeps zero days focusable and uses zero counts without data', () => {
    const columns = render(buckets());
    expect(columns.every((column) => column.props['aria-label'].endsWith('0 messages'))).toBe(true);
    expect(columns.every((column) => column.props.tabIndex === 0)).toBe(true);
    act(() => columns[0].props.onFocus({ currentTarget: target }));
    expect(renderer.root.findByType(CareerTooltip).props.text).toBe('Feb 1, 2026 · 0 messages');
    expect(renderer.root.findByType(CareerTooltip).props.left).toBe(110);
    act(() => columns[0].props.onBlur());
    expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(0);
  });

  it('updates the hovered count when the selected family data changes', () => {
    const columns = render(buckets([['2026-03-02', 1200]]));
    act(() => columns[29].props.onMouseEnter({ currentTarget: target }));
    expect(renderer.root.findByType(CareerTooltip).props.text).toBe('Mar 2, 2026 · 1,200 messages');
    act(() =>
      renderer.update(<DailyActivityChart {...chartProps(buckets([['2026-03-02', 9]]))} />)
    );
    expect(renderer.root.findByType(CareerTooltip).props.text).toBe('Mar 2, 2026 · 9 messages');
    const root = renderer.root.find((node) => typeof node.props.onMouseLeave === 'function');
    act(() => root.props.onMouseLeave());
    expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(0);
  });

  it('requests a metric change and retains the controlled selection until the parent updates it', () => {
    const onMetricChange = vi.fn();
    const data = buckets([['2026-03-02', 12]]);
    const columns = render(data, { onMetricChange });
    const tabs = renderer.root.findAllByProps({ role: 'tab' });
    expect(tabs.map((tab) => tab.props['aria-selected'])).toEqual([true, false]);
    act(() => columns[29].props.onFocus({ currentTarget: target }));
    act(() => tabs[1].props.onClick());
    expect(onMetricChange).toHaveBeenCalledExactlyOnceWith('tokens');
    expect(tabs[0].props['aria-selected']).toBe(true);
    expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(0);
    act(() =>
      renderer.update(
        <DailyActivityChart {...chartProps(data, { metric: 'tokens', onMetricChange })} />
      )
    );
    expect(
      renderer.root.findAllByProps({ role: 'tab' }).map((tab) => tab.props['aria-selected'])
    ).toEqual([false, true]);
    const panel = renderer.root.findByProps({ role: 'tabpanel' });
    expect(panel.props['aria-labelledby']).toBe(
      renderer.root.findAllByProps({ role: 'tab' })[1].props.id
    );
  });

  it('stacks input below output with proportional daily and segment heights and exact tips', () => {
    const columns = render(buckets(), {
      metric: 'tokens',
      tokenDays: new Map([
        ['2026-02-01', { input: 30_000, output: 10_000 }],
        ['2026-03-02', { input: 10_000, output: 10_000 }],
        ['2026-01-31', { input: 100_000, output: 100_000 }],
      ]),
    });
    const stackClass = 'w-full flex flex-col-reverse rounded-t-sm overflow-hidden';
    const fullStack = columns[0].findByProps({ className: stackClass });
    expect(fullStack.props.style.height).toBe('100%');
    const fullSegments = fullStack.findAllByType('div').slice(1);
    expect(fullSegments.map((segment) => segment.props.style)).toEqual([
      { height: '75%', background: 'rgb(var(--accent-rgb) / 0.90)' },
      { height: '25%', background: 'rgb(var(--accent-rgb) / 0.48)' },
    ]);
    const halfStack = columns[29].findByProps({ className: stackClass });
    expect(halfStack.props.style.height).toBe('50%');
    expect(
      halfStack
        .findAllByType('div')
        .slice(1)
        .map((segment) => segment.props.style.height)
    ).toEqual(['50%', '50%']);
    act(() => columns[29].props.onMouseEnter({ currentTarget: target }));
    expect(renderer.root.findByType(CareerTooltip).props.text).toBe(
      'Mar 2, 2026 · Input 10,000 · Output 10,000 · Total 20,000'
    );
  });

  it('distinguishes recorded zero usage from days without samples', () => {
    const columns = render(buckets(), {
      metric: 'tokens',
      tokenDays: new Map([['2026-02-01', { input: 0, output: 0 }]]),
    });
    expect(columns[0].props['aria-label']).toBe('Feb 1, 2026 · Input 0 · Output 0 · Total 0');
    expect(columns[1].props['aria-label']).toBe('Feb 2, 2026 · Input — · Output — · Total —');
    expect(
      columns[0].findAll((node) => node.props.style?.background?.includes('--accent-rgb'))
    ).toHaveLength(0);
    expect(
      columns[1].findAll((node) => node.props.style?.background?.includes('--accent-rgb'))
    ).toHaveLength(0);
    act(() => columns[1].props.onFocus({ currentTarget: target }));
    expect(renderer.root.findByType(CareerTooltip).props.text).toBe(
      'Feb 2, 2026 · Input — · Output — · Total —'
    );
  });

  it.each(['idle', 'loading', 'error'] as const)(
    'keeps missing samples unknown during %s and retains cached bars',
    (tokenStatus) => {
      const onTokenRetry = vi.fn();
      const columns = render(buckets(), {
        metric: 'tokens',
        tokenStatus,
        onTokenRetry,
        tokenDays: new Map([['2026-03-02', { input: 100, output: 50 }]]),
      });
      expect(columns).toHaveLength(30);
      expect(columns[0].props['aria-label']).toBe('Feb 1, 2026 · Input — · Output — · Total —');
      expect(columns[29].props['aria-label']).toBe(
        'Mar 2, 2026 · Input 100 · Output 50 · Total 150'
      );
      expect(
        columns[29].findAll((node) => node.props.style?.background?.includes('--accent-rgb'))
      ).toHaveLength(2);
      const panel = renderer.root.findByProps({ role: 'tabpanel' });
      if (tokenStatus === 'error') {
        expect(panel.props['aria-busy']).toBe(false);
        const alert = panel.findByProps({ role: 'alert' });
        expect(alert.findByType('span').children).toContain('aiCareer.tokenLoadFailed');
        const retry = alert.findByType('button');
        expect(retry.children).toContain('aiCareer.retry');
        act(() => retry.props.onClick());
        expect(onTokenRetry).toHaveBeenCalledTimes(1);
      } else {
        expect(panel.props['aria-busy']).toBe(true);
        expect(panel.findByProps({ role: 'status' }).children).toContain('aiCareer.chartLoading');
        expect(onTokenRetry).not.toHaveBeenCalled();
      }
    }
  );

  it.each([
    ['ArrowRight', 0, 'tokens', 1],
    ['ArrowLeft', 0, 'tokens', 1],
    ['ArrowRight', 1, 'messages', 0],
    ['Home', 1, 'messages', 0],
    ['End', 0, 'tokens', 1],
  ] as const)('moves selection and keyboard focus with %s', (key, index, nextMetric, nextIndex) => {
    const onMetricChange = vi.fn();
    render(buckets(), { metric: index === 0 ? 'messages' : 'tokens', onMetricChange });
    const tabs = renderer.root.findAllByProps({ role: 'tab' });
    expect(tabs[index].props.tabIndex).toBe(0);
    expect(tabs[1 - index].props.tabIndex).toBe(-1);
    const focus = [vi.fn(), vi.fn()];
    const preventDefault = vi.fn();
    act(() =>
      tabs[index].props.onKeyDown({
        key,
        preventDefault,
        currentTarget: {
          parentElement: { querySelectorAll: () => focus.map((handler) => ({ focus: handler })) },
        },
      })
    );
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(onMetricChange).toHaveBeenCalledExactlyOnceWith(nextMetric);
    expect(focus[nextIndex]).toHaveBeenCalledTimes(1);
    expect(focus[1 - nextIndex]).not.toHaveBeenCalled();
  });

  it.each(['scroll', 'resize'] as const)(
    'positions a long token tip within the viewport and clears it on %s',
    (eventName) => {
      tooltipWidth = 440;
      const columns = render(buckets(), {
        metric: 'tokens',
        tokenDays: new Map([['2026-03-02', { input: 123_456_789, output: 987_654_321 }]]),
      });
      act(() =>
        columns[29].props.onFocus({
          currentTarget: {
            getBoundingClientRect: () => ({ left: 850, top: 250, width: 20 }),
          },
        })
      );
      const tooltip = renderer.root.findByType(CareerTooltip);
      expect(tooltip.props.fixed).toBe(true);
      expect(tooltip.props.left).toBe(736);
      expect(tooltip.props.top).toBe(246);
      expect(tooltip.props.text).toBe(
        'Mar 2, 2026 · Input 123,456,789 · Output 987,654,321 · Total 1,111,111,110'
      );
      const listeners = vi.mocked(window.addEventListener).mock.calls;
      const listener = listeners.find(([name]) => name === eventName)![1] as () => void;
      act(() => listener());
      expect(renderer.root.findAllByType(CareerTooltip)).toHaveLength(0);
      expect(window.removeEventListener).toHaveBeenCalledWith(
        eventName,
        listener,
        ...(eventName === 'scroll' ? [true] : [])
      );
    }
  );
});
