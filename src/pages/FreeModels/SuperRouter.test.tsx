import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import {
  FreeModelsMain,
  FreeModelsProvider,
  FreeModelsTitleActions,
  useFreeModels,
} from './FreeModels';

const { confirm, showToast } = vi.hoisted(() => ({ confirm: vi.fn(), showToast: vi.fn() }));
vi.mock('../../components', () => ({ getModelIcon: () => null }));
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => confirm }));
vi.mock('../../components/Toast', () => ({ useToast: () => ({ showToast }) }));
vi.mock('react-dom', () => ({ createPortal: (children: React.ReactNode) => children }));
vi.mock('../ModelNexus/context', () => ({ useModelNexus: () => ({}) }));
vi.mock('../../api/tauri', () => ({
  getSmartRouterConfig: vi.fn(),
  getSmartRouterCandidates: vi.fn(),
  getSmartRouterActivity: vi.fn(),
  setSmartRouterEnabled: vi.fn(),
  setSmartRouterCandidates: vi.fn(),
  removeSmartRouterCandidate: vi.fn(),
  getFreeModelDirectory: vi.fn(),
  getCachedFreeModelDirectory: vi.fn().mockResolvedValue(null),
}));

describe('Super Router static preview', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useFreeModels>;
  let showSmart = false;
  const config = {
    enabled: true,
    candidateIds: ['saved-model'],
    usableCandidateCount: 1,
    baseUrl: 'http://127.0.0.1:62185/v1',
    modelId: 'smart-router',
    port: 62185,
    running: true,
  };
  function Harness() {
    const value = useFreeModels();
    useLayoutEffect(() => {
      context = value;
    });
    return value.viewMode === 'super' || showSmart ? <FreeModelsMain /> : null;
  }
  async function open() {
    await act(async () => {
      renderer = create(
        <FreeModelsProvider>
          <FreeModelsTitleActions />
          <Harness />
        </FreeModelsProvider>
      );
    });
  }
  const select = async (mode: 'smart' | 'super') => {
    const button = renderer.root
      .findAllByType('button')
      .find((node) => node.children.includes(`freeModels.view.${mode}`))!;
    await act(async () => button.props.onClick());
  };
  beforeEach(() => {
    showSmart = false;
    vi.mocked(api.getSmartRouterConfig).mockResolvedValue(config);
    vi.mocked(api.getSmartRouterCandidates).mockResolvedValue([
      {
        internalId: 'saved-model',
        name: 'Saved model',
        modelId: 'custom',
        baseUrl: 'https://example/v1',
        apiKey: '',
      },
    ]);
    vi.mocked(api.getSmartRouterActivity).mockResolvedValue({
      candidateId: null,
      active: false,
      sequence: 0,
      updatedAtMs: 0,
    });
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    vi.clearAllMocks();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('shows seven fixed cards with a disabled hub and no model or endpoint actions, even when real routing is enabled', async () => {
    await open();
    await select('super');
    const cards = renderer.root.findAll(
      (node) =>
        node.type === 'div' && String(node.props.className).includes('free-model-route-node')
    );
    expect(cards).toHaveLength(7);
    expect(cards.flatMap((card) => card.findAllByType('button'))).toHaveLength(0);
    const numbers = renderer.root.findAll(
      (node) =>
        node.type === 'span' && String(node.props.className).includes('free-model-route-priority')
    );
    expect(numbers.map((node) => node.children[0])).toEqual(['0', '1', '2', '3', '4', '5', '6']);
    expect(JSON.stringify(renderer.toJSON())).toContain('claude-opus-5-5');
    expect(JSON.stringify(renderer.toJSON())).toContain('freeModels.router.disabled');
    const hub = renderer.root.find(
      (node) =>
        node.type === 'div' && String(node.props.className).includes('free-model-router-hub')
    );
    expect(hub.findAllByType('button')).toHaveLength(0);
    const notice = renderer.root.find(
      (node) => node.type === 'span' && String(node.props.className).includes('text-red-500')
    );
    expect(notice.children).toEqual(['freeModels.super.unavailable']);
    expect(api.getSmartRouterActivity).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
    expect(showToast).not.toHaveBeenCalled();
  });

  it('switches only presentation and preserves saved candidates, enabled state and default tab', async () => {
    await open();
    expect(context.viewMode).toBe('smart');
    expect(api.getSmartRouterConfig).toHaveBeenCalledOnce();
    expect(api.getSmartRouterCandidates).toHaveBeenCalledOnce();
    const saved = context.customModels;
    await select('super');
    await select('smart');
    await select('super');
    expect(context.customModels).toBe(saved);
    expect([...context.selectedIds]).toEqual(['saved-model']);
    expect(context.routerEnabled).toBe(true);
    expect(api.getSmartRouterConfig).toHaveBeenCalledOnce();
    expect(api.getSmartRouterCandidates).toHaveBeenCalledOnce();
    expect(api.setSmartRouterEnabled).not.toHaveBeenCalled();
    expect(api.setSmartRouterCandidates).not.toHaveBeenCalled();
    expect(api.removeSmartRouterCandidate).not.toHaveBeenCalled();
    expect(api.getFreeModelDirectory).not.toHaveBeenCalled();
    expect(api.getSmartRouterActivity).not.toHaveBeenCalled();
  });

  it('stops real activity polling on Super and resumes it with the saved Smart cards and copy controls', async () => {
    showSmart = true;
    vi.useFakeTimers();
    vi.stubGlobal('window', { setInterval, clearInterval });
    vi.stubGlobal('document', { body: {} });
    vi.stubGlobal('requestAnimationFrame', () => 1);
    vi.stubGlobal('cancelAnimationFrame', vi.fn());
    await open();
    expect(api.getSmartRouterActivity).toHaveBeenCalledOnce();
    await act(async () => {
      vi.advanceTimersByTime(750);
    });
    expect(api.getSmartRouterActivity).toHaveBeenCalledTimes(2);
    await select('super');
    await act(async () => {
      vi.advanceTimersByTime(1500);
    });
    expect(api.getSmartRouterActivity).toHaveBeenCalledTimes(2);
    await select('smart');
    expect(api.getSmartRouterActivity).toHaveBeenCalledTimes(3);
    expect(JSON.stringify(renderer.toJSON())).toContain('Saved model');
    expect(
      renderer.root
        .findAllByType('button')
        .filter((node) => String(node.props['aria-label']).startsWith('btn.copy'))
    ).toHaveLength(2);
    expect(api.setSmartRouterEnabled).not.toHaveBeenCalled();
    expect(api.setSmartRouterCandidates).not.toHaveBeenCalled();
  });
});
