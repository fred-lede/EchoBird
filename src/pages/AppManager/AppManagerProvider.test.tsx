import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import { useToolsStore } from '../../stores/toolsStore';
import { useNavigationStore } from '../../stores/navigationStore';
import { AppManagerProvider } from './AppManagerProvider';
import { AppManagerContext, useAppManager } from './context';
import { AppManagerBottom } from './AppManagerComponents';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import museCodeDefinition from '../../../tools/musecode/paths.json';

vi.hoisted(() => vi.stubGlobal('__APP_EDITION__', 'full'));
vi.mock('../../utils/platform', () => ({ IS_WINDOWS: false }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn().mockResolvedValue('/fixture') }));
vi.mock('../../hooks/useI18n', () => {
  const t = (key: string) => key;
  return { useI18n: () => ({ t, locale: 'en' }) };
});
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => async () => true }));
vi.mock('../../components', () => ({ EFFORT_PULSE_ONESHOT_MS: 0 }));
vi.mock('../ModelNexus/context', () => {
  const userModels: never[] = [];
  return { useModelNexus: () => ({ userModels }) };
});
vi.mock('../FreeModels', () => ({ useFreeModels: () => ({ routerEnabled: false }) }));
vi.mock('./useClaudeCodeAccounts', () => ({ useClaudeCodeAccounts: () => ({ accounts: [] }) }));
vi.mock('./useWorkBuddyAccounts', () => ({ useWorkBuddyAccounts: () => ({ accounts: [] }) }));
vi.mock('./useDeepSeekAccounts', () => ({ useDeepSeekAccounts: () => ({ accounts: [] }) }));
vi.mock('../../api/tauri', () => ({
  listClineAccounts: vi.fn().mockResolvedValue([]),
  startClineLogin: vi.fn(),
  pollClineLogin: vi.fn(),
  cancelClineLogin: vi.fn().mockResolvedValue(undefined),
  switchClineAccount: vi.fn(),
  refreshClineAccount: vi.fn(),
  deleteClineAccount: vi.fn(),
  listKiloAccounts: vi.fn().mockResolvedValue([]),
  startKiloLogin: vi.fn(),
  pollKiloLogin: vi.fn(),
  cancelKiloLogin: vi.fn().mockResolvedValue(undefined),
  switchKiloAccount: vi.fn(),
  refreshKiloAccount: vi.fn(),
  deleteKiloAccount: vi.fn(),
  listCopilotAccounts: vi.fn().mockResolvedValue([]),
  startCopilotLogin: vi.fn(),
  pollCopilotLogin: vi.fn(),
  cancelCopilotLogin: vi.fn().mockResolvedValue(undefined),
  switchCopilotAccount: vi.fn(),
  refreshCopilotAccount: vi.fn(),
  deleteCopilotAccount: vi.fn(),
  listKiroAccounts: vi.fn().mockResolvedValue([]),
  startKiroLogin: vi.fn(),
  pollKiroLogin: vi.fn(),
  cancelKiroLogin: vi.fn().mockResolvedValue(undefined),
  switchKiroAccount: vi.fn(),
  refreshKiroAccountQuota: vi.fn(),
  deleteKiroAccount: vi.fn(),
  listTraeAccounts: vi.fn().mockResolvedValue([]),
  startTraeLogin: vi.fn(),
  pollTraeLogin: vi.fn(),
  cancelTraeLogin: vi.fn().mockResolvedValue(undefined),
  switchTraeAccount: vi.fn(),
  refreshTraeAccountQuota: vi.fn(),
  claimTraeDailyCredits: vi.fn(),
  deleteTraeAccount: vi.fn(),
  listZCodeAccounts: vi.fn(),
  startZCodeLogin: vi.fn(),
  pollZCodeLogin: vi.fn(),
  cancelZCodeLogin: vi.fn(),
  switchZCodeAccount: vi.fn(),
  refreshZCodeAccountQuota: vi.fn(),
  deleteZCodeAccount: vi.fn(),

  applyModelToTool: vi.fn(),
  restoreToolToOfficial: vi.fn(),
  startTool: vi.fn().mockResolvedValue({ success: true }),
  listCodexAccounts: vi.fn(),
  cancelCodexLogin: vi.fn(),
  startCodexLogin: vi.fn(),
  refreshCodexAccountQuota: vi.fn(),
  getModels: vi.fn().mockResolvedValue([]),
  getInstallIndex: vi.fn().mockResolvedValue('{"ids":[]}'),
  listGrokBotAccounts: vi.fn().mockRejectedValue(new Error('accountError.read')),
  listCursorAccounts: vi.fn().mockRejectedValue(new Error('accountError.read')),
  listAntigravityAccounts: vi.fn().mockRejectedValue(new Error('accountError.read')),
  startAntigravityLogin: vi.fn(),
  pollAntigravityLogin: vi.fn(),
  cancelAntigravityLogin: vi.fn().mockResolvedValue(undefined),
  deleteAntigravityAccount: vi.fn(),
  switchAntigravityAccount: vi.fn(),
  refreshAntigravityAccount: vi.fn(),
  openExternal: vi.fn().mockResolvedValue(undefined),
  startGrokBotLogin: vi.fn(),
  pollGrokBotLogin: vi.fn(),
  cancelGrokBotLogin: vi.fn(),
  deleteGrokBotAccount: vi.fn(),
  startCursorLogin: vi.fn(),
  pollCursorLogin: vi.fn(),
  cancelCursorLogin: vi.fn(),
  deleteCursorAccount: vi.fn(),
  refreshGrokBotAccount: vi.fn(),
  refreshCursorAccount: vi.fn(),
  listGrokAccounts: vi.fn().mockRejectedValue(new Error('accountError.read')),
  listCueAccounts: vi.fn().mockRejectedValue(new Error('accountError.read')),
  startCueLogin: vi.fn(),
  pollCueLogin: vi.fn(),
  cancelCueLogin: vi.fn(),
  switchCueAccount: vi.fn(),
  deleteCueAccount: vi.fn(),
  refreshCueAccount: vi.fn(),
  startGrokLogin: vi.fn(),
  pollGrokLogin: vi.fn(),
  cancelGrokLogin: vi.fn(),
  deleteGrokAccount: vi.fn(),
  switchGrokAccount: vi.fn(),
  refreshGrokAccount: vi.fn(),
  listClaudeDesktopAccounts: vi.fn().mockResolvedValue([]),
  startClaudeDesktopLogin: vi.fn(),
  pollClaudeDesktopLogin: vi.fn(),
  cancelClaudeDesktopLogin: vi.fn(),
  refreshClaudeDesktopAccount: vi.fn(),
  switchClaudeDesktopAccount: vi.fn(),
  deleteClaudeDesktopAccount: vi.fn(),
  listManusAccounts: vi.fn().mockRejectedValue(new Error('accountError.read')),
  startManusLogin: vi.fn(),
  pollManusLogin: vi.fn(),
  cancelManusLogin: vi.fn(),
  deleteManusAccount: vi.fn(),
  switchManusAccount: vi.fn(),
  refreshManusAccount: vi.fn(),
}));

describe('Claude Desktop account apply', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    const storage = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    });
    const account = { id: 'desktop-saved', email: 'one@example.test', active: true };
    vi.mocked(api.listClaudeDesktopAccounts).mockResolvedValue([account]);
    vi.mocked(api.switchClaudeDesktopAccount).mockResolvedValue(account);
    vi.mocked(api.restoreToolToOfficial).mockResolvedValue({ success: true, message: 'ok' });
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore
      .getState()
      .setDetectedTools([
        { id: 'claudedesktop', name: 'Claude', category: 'Desktop', installed: true },
      ]);
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.mocked(api.listClaudeDesktopAccounts).mockResolvedValue([]);
  });
  async function mount() {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    await act(async () => {
      context.setSelectedTool('claudedesktop');
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
  it('requires explicit apply and respects the launch-after-apply preference', async () => {
    await mount();
    expect(context.claudeDesktopAccounts.selectedId).toBe('desktop-saved');
    expect(api.switchClaudeDesktopAccount).not.toHaveBeenCalled();
    expect(api.restoreToolToOfficial).not.toHaveBeenCalled();
    act(() => context.setLaunchAfterApply(false));
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.switchClaudeDesktopAccount).toHaveBeenCalledWith('desktop-saved');
    expect(api.restoreToolToOfficial).toHaveBeenCalledWith('claudedesktop');
    expect(api.listClaudeDesktopAccounts).toHaveBeenCalledTimes(2);
    expect(api.startTool).not.toHaveBeenCalled();
    expect(api.refreshClaudeDesktopAccount).not.toHaveBeenCalled();
    act(() => context.setLaunchAfterApply(true));
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.startTool).toHaveBeenCalledWith('claudedesktop', undefined);
    expect(openDialog).not.toHaveBeenCalled();
    expect(context.isLaunching).toBe(false);
  });
  it('does not restore provider settings or launch when normal exit is required', async () => {
    await mount();
    expect(context.claudeDesktopAccounts.selectedId).toBe('desktop-saved');
    vi.mocked(api.switchClaudeDesktopAccount).mockRejectedValueOnce(
      new Error('accountError.quitClaudeDesktop')
    );
    await act(async () => {
      await context.handleLaunch();
    });
    expect(context.applyError).toBe('accountError.quitClaudeDesktop');
    expect(context.isLaunching).toBe(false);
    expect(api.restoreToolToOfficial).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
  });
});

describe.each(['codex', 'chatgptdesktop'])('%s Web Search preference', (tool) => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  let storage: Map<string, string>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    storage = new Map();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    });
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore.getState().setDetectedTools(
      ['codex', 'chatgptdesktop'].map((id) => ({
        id,
        name: id,
        category: 'Desktop',
        installed: true,
        apiProtocol: ['openai'],
      }))
    );
    vi.mocked(api.listCodexAccounts).mockResolvedValue([]);
    vi.mocked(api.getModels).mockResolvedValue([
      {
        internalId: 'fixture',
        name: 'Fixture',
        modelId: 'fixture-model',
        baseUrl: 'https://provider.example/v1',
        apiKey: 'fixture',
      },
      {
        internalId: 'smart-router',
        name: 'Auto Router',
        modelId: 'auto',
        baseUrl: 'http://127.0.0.1:53683/v1',
        apiKey: 'fixture-router',
      },
      ...['deepseek.com', 'xiaomimimo.com'].map((domain) => ({
        internalId: domain,
        name: domain,
        modelId: 'fixture-model',
        baseUrl: `https://api.${domain}/v1`,
        apiKey: 'fixture',
      })),
    ]);
    vi.mocked(api.applyModelToTool).mockResolvedValue({ success: true, message: 'ok' });
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.mocked(api.getModels).mockResolvedValue([]);
    vi.stubGlobal('localStorage', undefined);
    vi.useRealTimers();
  });
  const mount = async () => {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    await act(async () => {
      context.setSelectedTool(tool);
    });
    act(() => {
      context.handleSelectModel(tool, 'fixture');
      context.setAgreedConfigPolicy(true);
      context.setLaunchAfterApply(false);
    });
  };
  it('restores the default-on preference without inferring provider capabilities', async () => {
    await mount();
    for (const id of ['fixture', 'deepseek.com', 'xiaomimimo.com']) {
      act(() => context.handleSelectModel(tool, id));
      expect(context.codexWebSearch).toBe(true);
      await act(async () => {
        await context.handleLaunch();
        await vi.advanceTimersByTimeAsync(3000);
      });
      expect(api.applyModelToTool).toHaveBeenLastCalledWith(
        tool,
        expect.objectContaining({ webSearch: true })
      );
    }
  });

  it('disables hosted search for auto without changing the saved preference for ordinary models', async () => {
    await mount();
    act(() => context.handleSelectModel(tool, 'smart-router'));
    await act(async () => {
      await context.handleLaunch();
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(api.applyModelToTool).toHaveBeenLastCalledWith(
      tool,
      expect.objectContaining({ model: 'auto', webSearch: false })
    );
    const calls = vi.mocked(api.applyModelToTool).mock.calls.length;
    act(() => context.setCodexWebSearch(true));
    expect(api.applyModelToTool).toHaveBeenCalledTimes(calls);
    expect(context.codexWebSearch).toBe(true);
    expect(storage.has('echobird_codex_web_search')).toBe(false);
    act(() => context.handleSelectModel(tool, 'fixture'));
    await act(async () => {
      await context.handleLaunch();
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(api.applyModelToTool).toHaveBeenLastCalledWith(
      tool,
      expect.objectContaining({ model: 'fixture-model', webSearch: true })
    );
  });
  it('keeps navigation passive and preserves off across repeat apply, tool switches and remounts', async () => {
    await mount();
    await act(async () => context.setCodexWebSearch(false));
    expect(api.applyModelToTool).toHaveBeenCalledExactlyOnceWith(
      tool,
      expect.objectContaining({ webSearch: false })
    );
    vi.mocked(api.applyModelToTool).mockClear();
    const other = tool === 'codex' ? 'chatgptdesktop' : 'codex';
    await act(async () => {
      context.setSelectedTool(other);
      context.handleSelectModel(other, 'fixture');
    });
    act(() => context.setViewMode('install'));
    act(() => useNavigationStore.getState().setActivePage('myProjects'));
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
    expect(api.startCodexLogin).not.toHaveBeenCalled();
    expect(api.refreshCodexAccountQuota).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
    expect(context.applyError).toBeNull();
    act(() => renderer.unmount());
    act(() => useNavigationStore.getState().setActivePage('apps'));
    await mount();
    expect(context.codexWebSearch).toBe(false);
    for (let i = 0; i < 2; i++) {
      await act(async () => {
        await context.handleLaunch();
        await vi.advanceTimersByTimeAsync(3000);
      });
      expect(api.applyModelToTool).toHaveBeenLastCalledWith(
        tool,
        expect.objectContaining({ webSearch: false })
      );
    }
    await act(async () => {
      context.setSelectedTool(other);
    });
    act(() => context.handleSelectModel(other, 'fixture'));
    await act(async () => context.setCodexWebSearch(true));
    expect(api.applyModelToTool).toHaveBeenLastCalledWith(
      other,
      expect.objectContaining({ webSearch: true })
    );
    expect(api.startTool).not.toHaveBeenCalled();
  });
  it('shows explicit apply failures without losing the saved choice or launching', async () => {
    await mount();
    vi.mocked(api.applyModelToTool).mockResolvedValueOnce({
      success: false,
      message: 'Config unavailable',
    });
    await act(async () => context.setCodexWebSearch(false));
    expect(context.applyError).toBe('Config unavailable');
    expect(context.isLaunching).toBe(false);
    expect(context.codexWebSearch).toBe(false);
    expect(storage.get('echobird_codex_web_search')).toBe('false');
    expect(api.startTool).not.toHaveBeenCalled();
  });
  it('blocks overlapping flips while the selected API model is being updated', async () => {
    await mount();
    let resolve!: (value: { success: boolean; message: string }) => void;
    vi.mocked(api.applyModelToTool).mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    act(() => context.setCodexWebSearch(false));
    expect(context.isLaunching).toBe(true);
    act(() => context.setCodexWebSearch(true));
    expect(context.codexWebSearch).toBe(false);
    expect(api.applyModelToTool).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolve({ success: true, message: 'ok' });
    });
    expect(context.isLaunching).toBe(false);
  });
  it('only saves the preference when an account or uninstalled client is selected', async () => {
    await mount();
    act(() => context.setSelectedCodexAccountId('saved-account'));
    await act(async () => context.setCodexWebSearch(false));
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    act(() => context.handleSelectModel(tool, 'fixture'));
    act(() =>
      useToolsStore
        .getState()
        .setDetectedTools([{ id: tool, name: tool, category: 'Desktop', installed: false }])
    );
    await act(async () => context.setCodexWebSearch(true));
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(storage.get('echobird_codex_web_search')).toBe('true');
  });
});

describe.each(['cline', 'clinedesktop'])('%s API model workflow', (tool) => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore.getState().setDetectedTools([
      {
        id: 'cline',
        name: 'Cline CLI',
        category: 'CLI Code',
        installed: true,
        activeModel: 'cli-before',
        apiProtocol: ['openai', 'anthropic'],
        startCommand: 'cline',
      },
      {
        id: 'clinedesktop',
        name: 'Cline Desktop',
        category: 'Desktop',
        installed: true,
        activeModel: 'desktop-before',
        apiProtocol: ['openai', 'anthropic'],
      },
    ]);
    vi.mocked(api.getModels).mockResolvedValue([
      {
        internalId: 'fixture',
        name: 'Fixture',
        modelId: 'fixture-model',
        baseUrl: 'http://127.0.0.1:1234/v1',
        apiKey: 'fixture',
      },
    ]);
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.mocked(api.getModels).mockResolvedValue([]);
    vi.useRealTimers();
  });
  const mount = async () => {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => {
      context.setSelectedTool(tool);
      context.setAgreedConfigPolicy(true);
      context.setLaunchAfterApply(true);
    });
  };
  it('keeps tool, tab and page navigation passive, including uninstalled clients', async () => {
    await mount();
    act(() => context.handleSelectModel(tool, 'fixture'));
    act(() => context.setSelectedTool(tool === 'cline' ? 'clinedesktop' : 'cline'));
    act(() => context.setViewMode('install'));
    act(() => useNavigationStore.getState().setActivePage('myProjects'));
    act(() =>
      useToolsStore
        .getState()
        .setDetectedTools([{ id: tool, name: tool, category: 'Desktop', installed: false }])
    );
    act(() => {
      useNavigationStore.getState().setActivePage('apps');
      context.setSelectedTool(tool);
    });
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
    expect(api.listCodexAccounts).not.toHaveBeenCalled();
    expect(api.listCursorAccounts).not.toHaveBeenCalled();
    expect(api.startGrokLogin).not.toHaveBeenCalled();
    expect(api.refreshGrokAccount).not.toHaveBeenCalled();
    expect(context.applyError).toBeNull();
  });
  it('waits for explicit apply, updates both shared selectors and honors launch unchecked', async () => {
    await mount();
    act(() => {
      context.handleSelectModel(tool, 'fixture');
      context.setLaunchAfterApply(false);
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    let resolve!: (value: { success: boolean; message: string }) => void;
    vi.mocked(api.applyModelToTool).mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    let pending!: Promise<void>;
    act(() => {
      pending = context.handleLaunch();
    });
    expect(context.isLaunching).toBe(true);
    expect(api.startTool).not.toHaveBeenCalled();
    await act(async () => {
      resolve({ success: true, message: 'ok' });
      await pending;
    });
    expect(api.applyModelToTool).toHaveBeenCalledWith(
      tool,
      expect.objectContaining({ model: 'fixture-model', protocol: 'openai' })
    );
    expect(context.detectedTools.find((t) => t.id === tool)?.activeModel).toBe('fixture-model');
    const other = tool === 'cline' ? 'clinedesktop' : 'cline';
    expect(context.detectedTools.find((t) => t.id === other)?.activeModel).toBe('fixture-model');
    expect(context.toolModelConfig[other]).toBe('fixture');
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
  });
  it('does not launch or change the applied model when configuration fails', async () => {
    await mount();
    act(() => context.handleSelectModel(tool, 'fixture'));
    vi.mocked(api.applyModelToTool).mockResolvedValueOnce({
      success: false,
      message: 'Native storage unavailable',
    });
    await act(async () => {
      await context.handleLaunch();
    });
    expect(context.applyError).toBe('Native storage unavailable');
    expect(context.isLaunching).toBe(false);
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
    expect(context.detectedTools.find((t) => t.id === tool)?.activeModel).toBe(
      tool === 'cline' ? 'cli-before' : 'desktop-before'
    );
  });
  it('uses the shared launch flow and asks for a folder only for CLI', async () => {
    await mount();
    act(() => context.handleSelectModel(tool, 'fixture'));
    vi.mocked(api.applyModelToTool).mockResolvedValueOnce({ success: true, message: 'ok' });
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.startTool).toHaveBeenCalledWith(
      tool,
      tool === 'cline' ? 'cline' : undefined,
      tool === 'cline' ? '/fixture' : undefined
    );
    expect(openDialog).toHaveBeenCalledTimes(tool === 'cline' ? 1 : 0);
  });
});

describe('Muse Code API model workflow', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    const storage = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    });
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore.getState().setDetectedTools([
      { ...museCodeDefinition, id: 'musecode', installed: true, activeModel: 'before' },
      { id: 'muse', name: 'Muse', category: 'Cloud Agent', installed: true, noModelConfig: true },
    ]);
    vi.mocked(api.getModels).mockResolvedValue([
      {
        internalId: 'fixture',
        name: 'Fixture',
        modelId: 'fixture-model',
        baseUrl: 'https://example.test/v1',
        apiKey: 'fixture-key',
      },
    ]);
    vi.mocked(api.applyModelToTool).mockResolvedValue({ success: true, message: 'ok' });
    vi.mocked(openDialog).mockResolvedValue('/fixture');
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.mocked(api.getModels).mockResolvedValue([]);
    vi.useRealTimers();
  });
  const mount = async () => {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => context.setSelectedTool('musecode'));
  };

  it('keeps selection, tool, tab and page navigation passive, including an uninstalled client', async () => {
    await mount();
    act(() => context.handleSelectModel('musecode', 'fixture'));
    act(() => context.setSelectedTool('muse'));
    act(() => context.setViewMode('install'));
    act(() => useNavigationStore.getState().setActivePage('models'));
    act(() =>
      useToolsStore
        .getState()
        .setDetectedTools([{ ...museCodeDefinition, id: 'musecode', installed: false }])
    );
    act(() => {
      useNavigationStore.getState().setActivePage('apps');
      context.setSelectedTool('musecode');
    });
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.restoreToolToOfficial).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
    expect(api.listCodexAccounts).not.toHaveBeenCalled();
    expect(api.listCursorAccounts).not.toHaveBeenCalled();
    expect(api.startCodexLogin).not.toHaveBeenCalled();
    expect(api.refreshCodexAccountQuota).not.toHaveBeenCalled();
    expect(context.applyError).toBeNull();
  });

  it('applies the pending model explicitly without launching when launch is unchecked', async () => {
    await mount();
    act(() => {
      context.handleSelectModel('musecode', 'fixture');
      context.setLaunchAfterApply(false);
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.applyModelToTool).toHaveBeenCalledExactlyOnceWith('musecode', {
      id: 'fixture',
      name: 'Fixture',
      baseUrl: 'https://example.test/v1',
      apiKey: 'fixture-key',
      model: 'fixture-model',
      protocol: 'openai',
    });
    expect(context.detectedTools.find((tool) => tool.id === 'musecode')?.activeModel).toBe(
      'fixture-model'
    );
    expect(context.toolModelConfig.muse).toBeUndefined();
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
  });

  it('waits for configuration before choosing a working folder and launching', async () => {
    await mount();
    act(() => context.handleSelectModel('musecode', 'fixture'));
    let resolve!: (value: { success: boolean; message: string }) => void;
    vi.mocked(api.applyModelToTool).mockImplementationOnce(() => new Promise((r) => (resolve = r)));
    let pending!: Promise<void>;
    act(() => {
      pending = context.handleLaunch();
    });
    expect(context.isLaunching).toBe(true);
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
    await act(async () => {
      resolve({ success: true, message: 'ok' });
      await pending;
    });
    expect(api.startTool).toHaveBeenCalledExactlyOnceWith('musecode', 'muse', '/fixture');
    expect(openDialog).toHaveBeenCalledExactlyOnceWith({ directory: true, multiple: false });
    expect(vi.mocked(api.applyModelToTool).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(api.startTool).mock.invocationCallOrder[0]
    );
  });

  it('preserves the applied model and stops launch when configuration fails', async () => {
    await mount();
    act(() => context.handleSelectModel('musecode', 'fixture'));
    vi.mocked(api.applyModelToTool).mockResolvedValueOnce({
      success: false,
      message: 'Native storage unavailable',
    });
    await act(async () => {
      await context.handleLaunch();
    });
    expect(context.applyError).toBe('Native storage unavailable');
    expect(context.isLaunching).toBe(false);
    expect(context.toolModelConfig.musecode).toBe('fixture');
    expect(context.detectedTools.find((tool) => tool.id === 'musecode')?.activeModel).toBe(
      'before'
    );
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
  });

  it('can launch without a model and aborts when the working folder is cancelled', async () => {
    await mount();
    vi.mocked(openDialog).mockResolvedValueOnce(null);
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
    expect(context.isLaunching).toBe(false);
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.startTool).toHaveBeenCalledExactlyOnceWith('musecode', 'muse', '/fixture');
  });
});

describe('VS Code API model workflow', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    const storage = new Map<string, string>();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    });
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore.getState().setDetectedTools([
      {
        id: 'vscode',
        name: 'Visual Studio Code',
        category: 'IDE',
        installed: true,
        activeModel: 'before',
        apiProtocol: ['openai', 'anthropic'],
      },
      {
        id: 'cline',
        name: 'Cline',
        category: 'CLI Code',
        installed: true,
        activeModel: 'cline-before',
      },
      {
        id: 'copilotdesktop',
        name: 'GitHub Copilot',
        category: 'Desktop',
        installed: true,
      },
    ]);
    vi.mocked(api.getModels).mockResolvedValue([
      {
        internalId: 'fixture',
        name: 'Fixture',
        modelId: 'fixture-model',
        baseUrl: 'https://example.test/v1',
        apiKey: 'fixture-key',
      },
      {
        internalId: 'anthropic-fixture',
        name: 'Anthropic Fixture',
        modelId: 'anthropic-model',
        baseUrl: '',
        anthropicUrl: 'https://example.test/anthropic',
        apiKey: 'fixture-key',
      },
    ]);
    vi.mocked(api.applyModelToTool).mockResolvedValue({ success: true, message: 'ok' });
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.mocked(api.getModels).mockResolvedValue([]);
    vi.useRealTimers();
  });
  const mount = async () => {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => context.setSelectedTool('vscode'));
  };

  it('keeps selection and tool, tab and page navigation passive, including an uninstalled client', async () => {
    await mount();
    act(() => context.handleSelectModel('vscode', 'fixture'));
    act(() => context.setSelectedTool('cline'));
    act(() => context.setViewMode('install'));
    act(() => useNavigationStore.getState().setActivePage('myProjects'));
    act(() =>
      useToolsStore
        .getState()
        .setDetectedTools([
          { id: 'vscode', name: 'Visual Studio Code', category: 'IDE', installed: false },
        ])
    );
    act(() => {
      useNavigationStore.getState().setActivePage('apps');
      context.setViewMode('desktop');
      context.setSelectedTool('vscode');
    });
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.restoreToolToOfficial).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
    expect(api.listCodexAccounts).not.toHaveBeenCalled();
    expect(api.listCursorAccounts).not.toHaveBeenCalled();
    expect(api.startCodexLogin).not.toHaveBeenCalled();
    expect(api.refreshCodexAccountQuota).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
    expect(context.applyError).toBeNull();
  });

  it.each([
    ['fixture', 'openai', 'https://example.test/v1', 'fixture-model'],
    ['anthropic-fixture', 'anthropic', 'https://example.test/anthropic', 'anthropic-model'],
  ])(
    'applies %s explicitly and updates only VS Code without launching',
    async (id, protocol, baseUrl, model) => {
      await mount();
      act(() => {
        context.handleSelectModel('vscode', id);
        context.setLaunchAfterApply(false);
      });
      expect(api.applyModelToTool).not.toHaveBeenCalled();
      await act(async () => {
        await context.handleLaunch();
      });
      expect(api.applyModelToTool).toHaveBeenCalledWith('vscode', {
        id,
        name: id === 'fixture' ? 'Fixture' : 'Anthropic Fixture',
        baseUrl,
        apiKey: 'fixture-key',
        model,
        protocol,
      });
      expect(context.detectedTools.find((tool) => tool.id === 'vscode')?.activeModel).toBe(model);
      expect(context.detectedTools.find((tool) => tool.id === 'cline')?.activeModel).toBe(
        'cline-before'
      );
      expect(context.toolModelConfig.cline).toBeUndefined();
      expect(api.startTool).not.toHaveBeenCalled();
      expect(openDialog).not.toHaveBeenCalled();
    }
  );

  it('waits for configuration to finish before starting the existing IDE launch flow', async () => {
    await mount();
    act(() => context.handleSelectModel('vscode', 'fixture'));
    let resolve!: (value: { success: boolean; message: string }) => void;
    vi.mocked(api.applyModelToTool).mockImplementationOnce(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    let pending!: Promise<void>;
    act(() => {
      pending = context.handleLaunch();
    });
    expect(context.isLaunching).toBe(true);
    expect(api.startTool).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3500);
    });
    expect(context.isLaunching).toBe(true);
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.applyModelToTool).toHaveBeenCalledOnce();
    await act(async () => {
      resolve({ success: true, message: 'ok' });
      await pending;
    });
    expect(api.startTool).toHaveBeenCalledWith('vscode', undefined, undefined);
    expect(openDialog).not.toHaveBeenCalled();
    expect(context.isLaunching).toBe(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(context.isLaunching).toBe(false);
  });

  it('keeps the action pending until IDE launch finishes', async () => {
    await mount();
    act(() => context.handleSelectModel('vscode', 'fixture'));
    let resolve!: () => void;
    vi.mocked(api.startTool).mockImplementationOnce(
      () =>
        new Promise<void>((r) => {
          resolve = r;
        })
    );
    let pending!: Promise<void>;
    await act(async () => {
      pending = context.handleLaunch();
    });
    expect(api.startTool).toHaveBeenCalledOnce();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3500);
      await context.handleLaunch();
    });
    expect(context.isLaunching).toBe(true);
    expect(api.applyModelToTool).toHaveBeenCalledOnce();
    expect(api.startTool).toHaveBeenCalledOnce();
    await act(async () => {
      resolve();
      await pending;
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(context.isLaunching).toBe(false);
    expect(openDialog).not.toHaveBeenCalled();
  });

  it.each(['vscode', 'copilotdesktop'])(
    'ignores the previous failed tool cooldown while %s is pending',
    async (tool) => {
      await mount();
      act(() => {
        context.setSelectedTool('cline');
        context.handleSelectModel('cline', 'fixture');
      });
      vi.mocked(api.applyModelToTool).mockResolvedValueOnce({
        success: false,
        message: 'Unavailable',
      });
      await act(async () => {
        await context.handleLaunch();
      });
      expect(context.isLaunching).toBe(false);
      act(() => {
        context.setSelectedTool(tool);
        context.handleSelectModel(tool, 'fixture');
      });
      let resolve!: (value: { success: boolean; message: string }) => void;
      vi.mocked(api.applyModelToTool).mockImplementationOnce(
        () =>
          new Promise((r) => {
            resolve = r;
          })
      );
      let pending!: Promise<void>;
      act(() => {
        pending = context.handleLaunch();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3500);
      });
      expect(context.isLaunching).toBe(true);
      await act(async () => {
        await context.handleLaunch();
      });
      expect(api.applyModelToTool).toHaveBeenCalledTimes(2);
      expect(api.startTool).not.toHaveBeenCalled();
      await act(async () => {
        resolve({ success: true, message: 'ok' });
        await pending;
        await vi.advanceTimersByTimeAsync(3000);
      });
      expect(context.isLaunching).toBe(false);
      expect(api.startTool).toHaveBeenCalledExactlyOnceWith(tool, undefined, undefined);
    }
  );

  it('ignores a previous tool failure while VS Code configuration is pending', async () => {
    await mount();
    act(() => {
      context.setSelectedTool('cline');
      context.handleSelectModel('cline', 'fixture');
      context.setLaunchAfterApply(false);
    });
    let resolvePrevious!: (value: { success: boolean; message: string }) => void;
    vi.mocked(api.applyModelToTool).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolvePrevious = resolve;
        })
    );
    let previousPending!: Promise<void>;
    act(() => {
      previousPending = context.handleLaunch();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3500);
    });
    expect(context.isLaunching).toBe(false);
    act(() => {
      context.setSelectedTool('vscode');
      context.handleSelectModel('vscode', 'fixture');
    });
    let resolveCurrent!: (value: { success: boolean; message: string }) => void;
    vi.mocked(api.applyModelToTool).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCurrent = resolve;
        })
    );
    let currentPending!: Promise<void>;
    act(() => {
      currentPending = context.handleLaunch();
    });
    expect(context.isLaunching).toBe(true);
    await act(async () => {
      resolvePrevious({ success: false, message: 'Unavailable' });
      await previousPending;
    });
    await act(async () => {
      await context.handleLaunch();
    });
    const applyCountWhilePending = vi.mocked(api.applyModelToTool).mock.calls.length;
    await act(async () => {
      resolveCurrent({ success: true, message: 'ok' });
      await currentPending;
    });
    expect(applyCountWhilePending).toBe(2);
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
  });

  it.each(['cancel', 'error'])(
    'ignores the previous tool folder picker %s while VS Code configuration is pending',
    async (outcome) => {
      await mount();
      act(() => {
        context.setSelectedTool('cline');
        context.handleSelectModel('cline', 'fixture');
      });
      let resolvePrevious!: (value: { success: boolean; message: string }) => void;
      vi.mocked(api.applyModelToTool).mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolvePrevious = resolve;
          })
      );
      let previousPending!: Promise<void>;
      act(() => {
        previousPending = context.handleLaunch();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3500);
      });
      expect(context.isLaunching).toBe(false);
      act(() => {
        context.setSelectedTool('vscode');
        context.handleSelectModel('vscode', 'fixture');
        context.setLaunchAfterApply(false);
      });
      let resolveCurrent!: (value: { success: boolean; message: string }) => void;
      vi.mocked(api.applyModelToTool).mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveCurrent = resolve;
          })
      );
      let currentPending!: Promise<void>;
      act(() => {
        currentPending = context.handleLaunch();
      });
      expect(context.isLaunching).toBe(true);
      if (outcome === 'cancel') vi.mocked(openDialog).mockResolvedValueOnce(null);
      else vi.mocked(openDialog).mockRejectedValueOnce(new Error('Picker unavailable'));
      await act(async () => {
        resolvePrevious({ success: true, message: 'ok' });
        await previousPending;
      });
      expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: false });
      const launchingAfterPicker = context.isLaunching;
      await act(async () => {
        await context.handleLaunch();
      });
      const applyCountWhilePending = vi.mocked(api.applyModelToTool).mock.calls.length;
      await act(async () => {
        resolveCurrent({ success: true, message: 'ok' });
        await currentPending;
      });
      expect(applyCountWhilePending).toBe(2);
      expect(launchingAfterPicker).toBe(true);
      expect(api.startTool).not.toHaveBeenCalled();
    }
  );

  it('preserves the applied model and prevents launch when configuration fails', async () => {
    await mount();
    act(() => context.handleSelectModel('vscode', 'fixture'));
    vi.mocked(api.applyModelToTool).mockResolvedValueOnce({
      success: false,
      message: 'Invalid settings',
    });
    await act(async () => {
      await context.handleLaunch();
    });
    expect(context.applyError).toBe('Invalid settings');
    expect(context.isLaunching).toBe(false);
    expect(context.detectedTools.find((tool) => tool.id === 'vscode')?.activeModel).toBe('before');
    expect(api.startTool).not.toHaveBeenCalled();
    expect(openDialog).not.toHaveBeenCalled();
  });

  it('launches without writing configuration when the write policy is unchecked', async () => {
    await mount();
    act(() => {
      context.handleSelectModel('vscode', 'fixture');
      context.setAgreedConfigPolicy(false);
    });
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.startTool).toHaveBeenCalledWith('vscode', undefined, undefined);
    expect(openDialog).not.toHaveBeenCalled();
    expect(context.detectedTools.find((tool) => tool.id === 'vscode')?.activeModel).toBe('before');
  });
});

const accountTools = [
  'grokbot',
  'cursor',
  'grok',
  'manus',
  'cue',
  'antigravity',
  'antigravitydesktop',
] as const;
describe('Copilot Desktop pending API apply', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(async () => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    vi.mocked(api.listCopilotAccounts).mockResolvedValue([]);
    vi.mocked(api.getModels).mockResolvedValue([
      { internalId: 'fixture', name: 'Fixture', baseUrl: 'https://example.com/v1', apiKey: 'key' },
    ]);
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore
      .getState()
      .setDetectedTools([
        { id: 'copilotdesktop', name: 'GitHub Copilot', category: 'Desktop', installed: true },
      ]);
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => {
      context.setSelectedTool('copilotdesktop');
      context.setAgreedConfigPolicy(true);
      context.setLaunchAfterApply(true);
      context.handleSelectModel('copilotdesktop', 'fixture');
    });
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.mocked(api.getModels).mockResolvedValue([]);
    vi.useRealTimers();
  });
  it('blocks repeat applies until native initialization and optional launch finish', async () => {
    let finishApply!: (value: { success: boolean; message: string }) => void;
    let finishLaunch!: () => void;
    vi.mocked(api.applyModelToTool).mockReturnValueOnce(
      new Promise((resolve) => {
        finishApply = resolve;
      })
    );
    vi.mocked(api.startTool).mockReturnValueOnce(
      new Promise((resolve) => {
        finishLaunch = resolve;
      })
    );
    let pending!: Promise<void>;
    await act(async () => {
      pending = context.handleLaunch();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(context.isLaunching).toBe(true);
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.applyModelToTool).toHaveBeenCalledTimes(1);
    expect(api.startTool).not.toHaveBeenCalled();
    await act(async () => {
      finishApply({ success: true, message: 'ok' });
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(context.isLaunching).toBe(true);
    expect(api.startTool).toHaveBeenCalledTimes(1);
    await act(async () => {
      finishLaunch();
      await pending;
    });
    expect(context.isLaunching).toBe(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(context.isLaunching).toBe(false);
  });
});

describe.each(accountTools)('%s navigation account loading', (tool) => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  const list = {
    grokbot: api.listGrokBotAccounts,
    cursor: api.listCursorAccounts,
    grok: api.listGrokAccounts,
    manus: api.listManusAccounts,
    cue: api.listCueAccounts,
    antigravity: api.listAntigravityAccounts,
    antigravitydesktop: api.listAntigravityAccounts,
  }[tool];
  const setInstalled = (installed: boolean) =>
    useToolsStore
      .getState()
      .setDetectedTools([{ id: tool, name: tool, category: 'Desktop', installed }]);
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    useNavigationStore.getState().setActivePage('apps');
    setInstalled(false);
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.useRealTimers();
  });

  it('skips uninstalled clients and never turns navigation read failures into dialogs or quota refreshes', async () => {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => {
      context.setViewMode('install');
      context.setSelectedTool(tool);
    });
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(list).not.toHaveBeenCalled();
    expect(context.applyError).toBeNull();

    act(() => {
      setInstalled(true);
      context.setViewMode('desktop');
      context.setSelectedTool(tool);
    });
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(list).toHaveBeenCalledTimes(1);
    expect(context.applyError).toBeNull();

    act(() => context.setViewMode('install'));
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(list).toHaveBeenCalledTimes(1);
    expect(api.refreshGrokBotAccount).not.toHaveBeenCalled();
    expect(api.refreshCursorAccount).not.toHaveBeenCalled();
    expect(api.refreshGrokAccount).not.toHaveBeenCalled();
    expect(api.startGrokLogin).not.toHaveBeenCalled();
    expect(api.switchGrokAccount).not.toHaveBeenCalled();
    expect(api.refreshManusAccount).not.toHaveBeenCalled();
    expect(api.startManusLogin).not.toHaveBeenCalled();
    expect(api.switchManusAccount).not.toHaveBeenCalled();
    expect(api.refreshCueAccount).not.toHaveBeenCalled();
    expect(api.startCueLogin).not.toHaveBeenCalled();
    expect(api.switchCueAccount).not.toHaveBeenCalled();
    expect(api.refreshAntigravityAccount).not.toHaveBeenCalled();
    expect(api.startAntigravityLogin).not.toHaveBeenCalled();
    expect(api.switchAntigravityAccount).not.toHaveBeenCalled();
  });
});

describe('Cue account apply', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  const account = {
    id: 'cue-saved',
    email: 'cue@example.test',
    active: true,
    plan: 'Free',
    credits: null,
  };
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(async () => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    vi.mocked(api.listCueAccounts).mockResolvedValue([account]);
    vi.mocked(api.switchCueAccount).mockResolvedValue(account);
    vi.mocked(api.cancelCueLogin).mockResolvedValue(undefined);
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore
      .getState()
      .setDetectedTools([
        { id: 'cue', name: 'Cue', category: 'Cloud Agent', installed: true, noModelConfig: true },
      ]);
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    await act(async () => {
      context.setSelectedTool('cue');
    });
    await act(async () => {
      await vi.runOnlyPendingTimersAsync();
    });
    expect(context.cueAccounts.selectedId).toBe('cue-saved');
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
    vi.useRealTimers();
  });
  it('applies the selected Cue account before launch without touching Manus or API models', async () => {
    expect(api.switchCueAccount).not.toHaveBeenCalled();
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.switchCueAccount).toHaveBeenCalledExactlyOnceWith('cue-saved');
    expect(api.startTool).toHaveBeenCalledExactlyOnceWith('cue');
    expect(vi.mocked(api.switchCueAccount).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(api.startTool).mock.invocationCallOrder[0]
    );
    expect(api.switchManusAccount).not.toHaveBeenCalled();
    expect(api.applyModelToTool).not.toHaveBeenCalled();
    expect(api.refreshCueAccount).not.toHaveBeenCalled();
  });
  it('preserves selection and does not launch after a failed switch', async () => {
    vi.mocked(api.switchCueAccount).mockRejectedValueOnce(new Error('accountError.closeClient'));
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.startTool).not.toHaveBeenCalled();
    expect(context.cueAccounts.selectedId).toBe('cue-saved');
    expect(context.applyError).not.toBeNull();
    expect(context.isLaunching).toBe(false);
  });
  it('blocks applying or launching while the Cue login is pending', async () => {
    vi.mocked(api.startCueLogin).mockResolvedValue({
      loginId: 'cue-pending',
      expiresAt: Date.now() / 1000 + 60,
    });
    vi.mocked(api.pollCueLogin).mockReturnValue(new Promise(() => {}));
    await act(async () => {
      void context.cueAccounts.add();
    });
    expect(context.cueAccounts.busy).toBe(true);
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.switchCueAccount).not.toHaveBeenCalled();
    expect(api.startTool).not.toHaveBeenCalled();
  });
});

describe.each(['antigravity', 'antigravitydesktop'] as const)('%s account apply', (tool) => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useAppManager>;
  function Harness() {
    const state = useAppManager();
    useLayoutEffect(() => {
      context = state;
    });
    return null;
  }
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.listAntigravityAccounts).mockResolvedValue([
      { id: 'saved', email: 'one@example.test', active: true, plan: 'Pro', quotas: [] },
    ]);
    vi.mocked(api.switchAntigravityAccount).mockResolvedValue({
      id: 'saved',
      email: 'one@example.test',
      active: true,
      plan: 'Pro',
      quotas: [],
    });
    useNavigationStore.getState().setActivePage('apps');
    useToolsStore.getState().setDetectedTools([
      {
        id: tool,
        name: tool,
        category: tool === 'antigravity' ? 'CLI Code' : 'Desktop',
        installed: true,
        noModelConfig: true,
        startCommand: tool === 'antigravity' ? 'agy' : undefined,
      },
    ]);
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    useToolsStore.getState().setDetectedTools([]);
  });
  it('loads passively and applies only on explicit launch', async () => {
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => context.setSelectedTool(tool));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(context.antigravityAccounts.selectedId).toBe('saved');
    expect(api.switchAntigravityAccount).not.toHaveBeenCalled();
    expect(api.refreshAntigravityAccount).not.toHaveBeenCalled();
    vi.mocked(api.switchAntigravityAccount).mockRejectedValueOnce(new Error('accountError.write'));
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.startTool).not.toHaveBeenCalled();
    expect(context.applyError).toBe('accountError.write');
    await act(async () => {
      await context.handleLaunch();
    });
    expect(api.switchAntigravityAccount).toHaveBeenCalledWith('saved');
    expect(api.startTool).toHaveBeenCalledWith(
      tool,
      tool === 'antigravity' ? 'agy' : undefined,
      tool === 'antigravity' ? '/fixture' : undefined
    );
  });

  it('opens browser authorization without launching the client or disabling Launch', async () => {
    vi.mocked(api.startAntigravityLogin).mockResolvedValueOnce({
      loginId: 'browser-login',
      verificationUri: 'https://accounts.google.com/o/oauth2/v2/auth?state=fixture',
      expiresAt: Date.now() / 1000 + 60,
    });
    vi.mocked(api.pollAntigravityLogin).mockImplementation(() => new Promise(() => {}));
    await act(async () => {
      renderer = create(
        <AppManagerProvider>
          <Harness />
        </AppManagerProvider>
      );
    });
    act(() => context.setSelectedTool(tool));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    act(() => {
      void context.antigravityAccounts.add();
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(api.openExternal).toHaveBeenCalledWith(
      'https://accounts.google.com/o/oauth2/v2/auth?state=fixture'
    );
    expect(api.startTool).not.toHaveBeenCalled();
    expect(api.switchAntigravityAccount).not.toHaveBeenCalled();
    expect(context.antigravityAccounts.busy).toBe(true);
    let bottom!: ReactTestRenderer;
    act(() => {
      bottom = create(
        <AppManagerContext.Provider value={context}>
          <AppManagerBottom />
        </AppManagerContext.Provider>
      );
    });
    const launch = bottom.root
      .findAllByType('button')
      .find((button) => button.children.includes('btn.launchApp'));
    expect(launch?.props.disabled).toBe(false);
    act(() => bottom.unmount());
  });
});
