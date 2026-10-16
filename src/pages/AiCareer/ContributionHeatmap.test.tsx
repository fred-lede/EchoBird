import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, it, vi } from 'vitest';
import { useNavigationStore } from '../../stores/navigationStore';
import { ContributionHeatmap } from './ContributionHeatmap';
import { buildGrid, DAYS, WEEKS, type DayBuckets } from './heatmapData';

vi.mock('../../hooks/useI18n', () => ({
  useI18n: () => ({
    locale: 'en-US',
    t: (key: string) => (key === 'aiCareer.messages' ? '{count} messages' : key),
  }),
}));

let renderer: ReactTestRenderer;
afterEach(() => {
  act(() => renderer?.unmount());
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it('advances the calendar after reentering a new week with the same cached buckets', () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 9, 10, 12));
  vi.stubGlobal('window', { requestAnimationFrame: vi.fn(() => 1), cancelAnimationFrame: vi.fn() });
  vi.stubGlobal('getComputedStyle', () => ({ getPropertyValue: () => '' }));
  useNavigationStore.setState({ activePage: 'aiCareer' });
  const buckets: DayBuckets = {
    messages: new Map([
      ['2026-03-15', 4],
      ['2026-03-22', 8],
    ]),
    sessions: new Map([
      ['2026-03-15', 1],
      ['2026-03-22', 1],
    ]),
  };
  const target = { getBoundingClientRect: () => ({ left: 0, top: 40, width: 20 }) };
  act(() => {
    renderer = create(<ContributionHeatmap buckets={buckets} refreshKey={0} />, {
      createNodeMock: () => ({
        offsetWidth: 720,
        style: {},
        getBoundingClientRect: () => ({ left: 0, top: 0 }),
      }),
    });
  });
  const firstCell = () => renderer.root.findAll((node) => !!node.props.onMouseEnter)[0];
  act(() => firstCell().props.onMouseEnter({ currentTarget: target }));
  expect(renderer.root.findByProps({ role: 'tooltip' }).children).toEqual(['Mar 15 · 4 messages']);
  act(() => useNavigationStore.getState().setActivePage('apps'));
  vi.setSystemTime(new Date(2026, 9, 11, 12));
  act(() => useNavigationStore.getState().setActivePage('aiCareer'));
  expect(renderer.root.findByType(ContributionHeatmap).props.buckets).toBe(buckets);
  act(() => firstCell().props.onMouseEnter({ currentTarget: target }));
  expect(renderer.root.findByProps({ role: 'tooltip' }).children).toEqual(['Mar 22 · 8 messages']);
});

it('does not modify a supplied calendar date when projecting cached buckets', () => {
  const now = new Date(2026, 9, 11, 15, 37);
  const original = now.getTime();
  const grid = buildGrid({ messages: new Map(), sessions: new Map() }, now);
  expect(grid[29][6].date).toBe('2026-10-17');
  expect(now.getTime()).toBe(original);
});

function animationFixture(count: number) {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 9, 11, 12));
  let frameId = 0;
  const frames = new Map<number, FrameRequestCallback>();
  const request = vi.fn((callback: FrameRequestCallback) => {
    frames.set(++frameId, callback);
    return frameId;
  });
  const cancel = vi.fn((id: number) => frames.delete(id));
  vi.stubGlobal('window', { requestAnimationFrame: request, cancelAnimationFrame: cancel });
  vi.stubGlobal('getComputedStyle', () => ({ getPropertyValue: () => '' }));
  useNavigationStore.setState({ activePage: 'aiCareer' });
  const buckets = (messages: number): DayBuckets => ({
    messages: new Map(
      messages
        ? [
            ['2026-10-10', 16],
            ['2026-10-11', messages],
          ]
        : []
    ),
    sessions: new Map(
      messages
        ? [
            ['2026-10-10', 1],
            ['2026-10-11', 1],
          ]
        : []
    ),
  });
  const nodes = Array.from({ length: WEEKS * DAYS }, () => ({ style: { backgroundColor: '' } }));
  let nodeIndex = 0;
  act(() => {
    renderer = create(<ContributionHeatmap buckets={buckets(count)} refreshKey={0} />, {
      createNodeMock: (element) =>
        element.props.onMouseEnter
          ? nodes[nodeIndex++ % nodes.length]
          : { style: {}, offsetWidth: 720, getBoundingClientRect: () => ({ left: 0, top: 0 }) },
    });
  });
  return {
    request,
    cancel,
    nodes,
    update: (messages: number, refreshKey = 0) =>
      act(() =>
        renderer.update(<ContributionHeatmap buckets={buckets(messages)} refreshKey={refreshKey} />)
      ),
    frame: (time: number) => {
      const pending = [...frames.values()];
      frames.clear();
      act(() => pending.forEach((callback) => callback(time)));
    },
  };
}

it('keeps the intro running while background data changes and reveals the latest values', () => {
  const fixture = animationFixture(4);
  fixture.frame(100);
  fixture.frame(500);
  const today = fixture.nodes[(WEEKS - 1) * DAYS];
  const beforeUpdate = today.style.backgroundColor;
  const requests = fixture.request.mock.calls.length;
  fixture.update(20);
  expect(fixture.cancel).not.toHaveBeenCalled();
  expect(fixture.request).toHaveBeenCalledTimes(requests);
  expect(today.style.backgroundColor).toBe(beforeUpdate);
  fixture.update(20);
  expect(fixture.cancel).not.toHaveBeenCalled();
  fixture.frame(4000);
  expect(today.style.backgroundColor).toBe('rgb(var(--accent-rgb) / 0.96)');
  const cell = renderer.root.findAll((node) => !!node.props.onMouseEnter)[(WEEKS - 1) * DAYS];
  act(() =>
    cell.props.onMouseEnter({
      currentTarget: { getBoundingClientRect: () => ({ left: 0, top: 40, width: 20 }) },
    })
  );
  expect(renderer.root.findByProps({ role: 'tooltip' }).children).toEqual(['Oct 11 · 20 messages']);
});

it('updates a completed heatmap without starting another intro, including an empty result', () => {
  const fixture = animationFixture(4);
  fixture.frame(100);
  fixture.frame(4000);
  const requests = fixture.request.mock.calls.length;
  fixture.update(20);
  expect(fixture.request).toHaveBeenCalledTimes(requests);
  expect(fixture.cancel).not.toHaveBeenCalled();
  fixture.update(0);
  expect(fixture.request).toHaveBeenCalledTimes(requests);
});

it('starts once when an uncached empty heatmap receives its first data', () => {
  const fixture = animationFixture(0);
  expect(fixture.request).not.toHaveBeenCalled();
  fixture.update(4);
  expect(fixture.request).toHaveBeenCalledTimes(1);
  fixture.update(20);
  expect(fixture.request).toHaveBeenCalledTimes(1);
  expect(fixture.cancel).not.toHaveBeenCalled();
});

it('settles all cells when a background empty result arrives during the intro', () => {
  const fixture = animationFixture(4);
  fixture.frame(100);
  fixture.frame(500);
  expect(fixture.nodes[(WEEKS - 1) * DAYS].style.backgroundColor).not.toBe(
    'rgba(230, 228, 224, 0.060)'
  );
  const requests = fixture.request.mock.calls.length;
  fixture.update(0);
  expect(fixture.cancel).toHaveBeenCalledTimes(1);
  expect(fixture.request).toHaveBeenCalledTimes(requests);
  for (const node of fixture.nodes.slice(0, (WEEKS - 1) * DAYS + 1)) {
    expect(node.style.backgroundColor).toBe('rgb(var(--text-primary-rgb) / 0.06)');
  }
});

it('replays once for manual refresh and reentry, then cancels on leaving the page', () => {
  const fixture = animationFixture(4);
  expect(fixture.request).toHaveBeenCalledTimes(1);
  fixture.update(4, 1);
  expect(fixture.request).toHaveBeenCalledTimes(2);
  expect(fixture.cancel).toHaveBeenCalledTimes(1);
  fixture.update(20, 1);
  expect(fixture.request).toHaveBeenCalledTimes(2);
  act(() => useNavigationStore.getState().setActivePage('apps'));
  expect(fixture.cancel).toHaveBeenCalledTimes(2);
  act(() => useNavigationStore.getState().setActivePage('aiCareer'));
  expect(fixture.request).toHaveBeenCalledTimes(3);
});
