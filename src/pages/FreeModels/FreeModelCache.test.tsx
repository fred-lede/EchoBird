import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import type { FreeModelDirectory } from '../../api/freeModels';
import { useNavigationStore } from '../../stores/navigationStore';
import { FreeModelsPanel, FreeModelsProvider, useFreeModels } from './FreeModels';

const { showToast, confirm } = vi.hoisted(() => ({ showToast: vi.fn(), confirm: vi.fn() }));
vi.mock('../../components', () => ({ getModelIcon: () => null }));
vi.mock('../../components/Toast', () => ({ useToast: () => ({ showToast }) }));
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => confirm }));
vi.mock('../ModelNexus/context', () => ({
  useModelNexus: () => ({ userModels: [], isLoadingModels: false }),
}));
vi.mock('./AccountProxyPanel', () => ({
  AccountProxyPanel: () => null,
  openAccountModelForm: vi.fn(),
}));
vi.mock('../../api/tauri', () => ({
  getSmartRouterConfig: vi.fn(),
  getSmartRouterCandidates: vi.fn(),
  setSmartRouterEnabled: vi.fn(),
  setSmartRouterCandidates: vi.fn(),
  removeSmartRouterCandidate: vi.fn(),
  getFreeModelDirectory: vi.fn(),
  getCachedFreeModelDirectory: vi.fn(),
}));

function directory(modelId: string): FreeModelDirectory {
  return {
    version: 1,
    updatedAt: '2026-10-12',
    models: [
      {
        id: modelId,
        providerId: 'openrouter',
        provider: 'OpenRouter',
        modelId,
        baseUrl: 'https://example.test/v1',
        freeType: 'perpetual',
        freeTier: 'Free',
        rateLimits: '',
        notes: '',
        docsUrl: '',
        cardRequired: null,
        phoneRequired: null,
        commercialOk: null,
        verifiedAt: '2026-10-12',
      },
    ],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe('Free model directory cache', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useFreeModels>;
  const cached = directory('cached-free-model');
  const fresh = directory('fresh-free-model');

  function Harness() {
    const value = useFreeModels();
    useLayoutEffect(() => {
      context = value;
    });
    return <FreeModelsPanel />;
  }

  async function open() {
    await act(async () => {
      renderer = create(
        <FreeModelsProvider>
          <Harness />
        </FreeModelsProvider>
      );
    });
    await selectTab('free');
  }

  async function selectTab(tab: 'saved' | 'account' | 'free') {
    await act(async () => renderer.root.findByProps({ id: `router-tab-${tab}` }).props.onClick());
  }

  function fetchButton() {
    return renderer.root
      .findAllByType('button')
      .find((node) => node.children.includes('freeModels.fetch'))!;
  }

  async function clickFetch() {
    await act(async () => fetchButton().props.onClick());
  }

  function expectNoRouterWrites() {
    expect(api.setSmartRouterEnabled).not.toHaveBeenCalled();
    expect(api.setSmartRouterCandidates).not.toHaveBeenCalled();
    expect(api.removeSmartRouterCandidate).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  }

  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal('window', { setTimeout });
    vi.stubGlobal('localStorage', { getItem: vi.fn().mockReturnValue(null) });
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    useNavigationStore.setState({ activePage: 'freeModels' });
    vi.mocked(api.getCachedFreeModelDirectory).mockResolvedValue(cached);
    vi.mocked(api.getSmartRouterConfig).mockResolvedValue({
      enabled: false,
      candidateIds: [],
      usableCandidateCount: 0,
      baseUrl: 'http://127.0.0.1:62185/v1',
      modelId: 'smart-router',
      port: 62185,
      running: false,
    });
    vi.mocked(api.getSmartRouterCandidates).mockResolvedValue([]);
  });

  afterEach(() => {
    act(() => renderer?.unmount());
    vi.resetAllMocks();
    vi.restoreAllMocks();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('restores cached models across navigation and reopening without downloading or changing routes', async () => {
    await open();
    expect(context.models).toEqual(cached.models);
    expect(context.refreshing).toBe(false);
    expect(JSON.stringify(renderer.toJSON())).toContain('OpenRouter');
    await selectTab('saved');
    await selectTab('account');
    await selectTab('free');
    await act(async () => useNavigationStore.getState().setActivePage('models'));
    await act(async () => useNavigationStore.getState().setActivePage('freeModels'));
    await act(async () => context.setViewMode('super'));
    await act(async () => context.setViewMode('smart'));
    expect(context.models).toEqual(cached.models);
    expect(api.getCachedFreeModelDirectory).toHaveBeenCalledOnce();
    act(() => renderer.unmount());
    await open();
    expect(context.models).toEqual(cached.models);
    expect(api.getCachedFreeModelDirectory).toHaveBeenCalledTimes(2);
    expect(api.getFreeModelDirectory).not.toHaveBeenCalled();
    expect(showToast).not.toHaveBeenCalled();
    expectNoRouterWrites();
  });

  it('downloads only on the fetch action, keeps cached rows while connecting, and restores the new cache on reopening', async () => {
    const download = deferred<FreeModelDirectory>();
    vi.mocked(api.getFreeModelDirectory).mockReturnValue(download.promise);
    await open();
    await clickFetch();
    expect(api.getFreeModelDirectory).toHaveBeenCalledOnce();
    expect(fetchButton().props.disabled).toBe(true);
    expect(context.models).toEqual(cached.models);
    await act(async () => context.refresh());
    expect(api.getFreeModelDirectory).toHaveBeenCalledOnce();
    await act(async () => download.resolve(fresh));
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(context.models).toEqual(fresh.models);
    expect(fetchButton().props.disabled).toBe(false);
    vi.mocked(api.getCachedFreeModelDirectory).mockResolvedValue(fresh);
    act(() => renderer.unmount());
    await open();
    expect(context.models).toEqual(fresh.models);
    expect(api.getFreeModelDirectory).toHaveBeenCalledOnce();
    expect(showToast).not.toHaveBeenCalled();
    expectNoRouterWrites();
  });

  it.each(['missing', 'rejected'] as const)(
    'retains cached rows when an explicit download is %s',
    async (failure) => {
      if (failure === 'missing') vi.mocked(api.getFreeModelDirectory).mockResolvedValue(null);
      else vi.mocked(api.getFreeModelDirectory).mockRejectedValue(new Error('Offline'));
      await open();
      await clickFetch();
      expect(context.models).toEqual(cached.models);
      expect(fetchButton().props.disabled).toBe(false);
      expect(showToast).toHaveBeenCalledExactlyOnceWith(
        failure === 'missing' ? 'warning' : 'error',
        'freeModels.fetchFailed'
      );
      expectNoRouterWrites();
    }
  );

  it('uses a valid empty download to clear an outdated cached list', async () => {
    vi.mocked(api.getFreeModelDirectory).mockResolvedValue({ ...fresh, models: [] });
    await open();
    await clickFetch();
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(context.models).toEqual([]);
    expect(JSON.stringify(renderer.toJSON())).toContain('freeModels.fetchHint');
    expect(showToast).not.toHaveBeenCalled();
    expectNoRouterWrites();
  });

  it('does not let a delayed cache read replace a newer manual download', async () => {
    const cacheRead = deferred<FreeModelDirectory>();
    vi.mocked(api.getCachedFreeModelDirectory).mockReturnValue(cacheRead.promise);
    vi.mocked(api.getFreeModelDirectory).mockResolvedValue(fresh);
    await open();
    await clickFetch();
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(context.models).toEqual(fresh.models);
    await act(async () => cacheRead.resolve(cached));
    expect(context.models).toEqual(fresh.models);
    expect(showToast).not.toHaveBeenCalled();
  });

  it.each(['missing', 'rejected'] as const)(
    'opens quietly when the local cache is %s',
    async (failure) => {
      if (failure === 'missing') vi.mocked(api.getCachedFreeModelDirectory).mockResolvedValue(null);
      else
        vi.mocked(api.getCachedFreeModelDirectory).mockRejectedValue(new Error('Cache unreadable'));
      await open();
      expect(context.models).toEqual([]);
      expect(context.refreshing).toBe(false);
      expect(JSON.stringify(renderer.toJSON())).toContain('freeModels.fetchHint');
      expect(api.getFreeModelDirectory).not.toHaveBeenCalled();
      expect(showToast).not.toHaveBeenCalled();
      expectNoRouterWrites();
    }
  );

  it('ignores a pending cache read from a provider that has already closed', async () => {
    const previousRead = deferred<FreeModelDirectory>();
    vi.mocked(api.getCachedFreeModelDirectory).mockReturnValueOnce(previousRead.promise);
    await open();
    act(() => renderer.unmount());
    vi.mocked(api.getCachedFreeModelDirectory).mockResolvedValue(fresh);
    await open();
    await act(async () => previousRead.resolve(cached));
    expect(context.models).toEqual(fresh.models);
    expect(api.getFreeModelDirectory).not.toHaveBeenCalled();
    expect(showToast).not.toHaveBeenCalled();
    expectNoRouterWrites();
  });
});
