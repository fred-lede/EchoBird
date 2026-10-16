import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import * as accountApi from '../../api/accountProxy';
import { useNavigationStore } from '../../stores/navigationStore';
import { loadLocale, translate, type TKey } from '../../i18n';
import { FreeModelsPanel, FreeModelsProvider } from './FreeModels';

const { confirm, showToast, invoke, openExternal, language, context } = vi.hoisted(() => ({
  language: { locale: 'zh-Hans' },
  confirm: vi.fn(),
  showToast: vi.fn(),
  invoke: vi.fn(),
  openExternal: vi.fn(),
  context: {
    userModels: [],
    isLoadingModels: false,
    modelUsageData: {},
    refreshingUsageIds: new Set<string>(),
    isRefreshingUsage: false,
    refreshSingleUsage: vi.fn(),
    volcAkSkMissingIds: new Set<string>(),
    openAkskModal: vi.fn(),
    handleCardEdit: vi.fn(),
    handleCardDelete: vi.fn(),
    setNewModelForm: vi.fn(),
    setEditingModelId: vi.fn(),
    setModelModalDestination: vi.fn(),
    setShowAddModelModal: vi.fn(),
    setKeyDestroyed: vi.fn(),
    setShowApiKey: vi.fn(),
  },
}));
vi.mock('../../components', () => ({ getModelIcon: () => null }));
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => confirm }));
vi.mock('../../components/Toast', () => ({ useToast: () => ({ showToast }) }));
vi.mock('../../hooks/useI18n', () => ({
  useI18n: () => ({
    locale: language.locale,
    t: (key: TKey) =>
      key.startsWith('accountProxy.risk') || key.startsWith('freeModels.tab.')
        ? translate(key, language.locale)
        : key,
  }),
}));
vi.mock('../ModelNexus/context', () => ({ useModelNexus: () => context }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: openExternal }));
vi.mock('../../api/tauri', () => ({
  getSmartRouterConfig: vi.fn(),
  getSmartRouterCandidates: vi.fn(),
  setSmartRouterEnabled: vi.fn(),
  setSmartRouterCandidates: vi.fn(),
  removeSmartRouterCandidate: vi.fn(),
  getFreeModelDirectory: vi.fn(),
  getCachedFreeModelDirectory: vi.fn().mockResolvedValue(null),
}));
vi.mock('../../api/accountProxy', async (original) => ({
  ...(await original<typeof import('../../api/accountProxy')>()),
  listProxyAccounts: vi.fn(),
  getAccountProxyModels: vi.fn(),
}));

const storageKey = 'echobird-account-proxy-risk-acknowledged';
let renderer: ReactTestRenderer;
let stored: Map<string, string>;
let getItem: ReturnType<typeof vi.fn>;
let setItem: ReturnType<typeof vi.fn>;
let fetch: ReturnType<typeof vi.fn>;

async function open() {
  await act(async () => {
    renderer = create(
      <FreeModelsProvider>
        <FreeModelsPanel />
      </FreeModelsProvider>
    );
  });
}
async function selectTab(tab: 'saved' | 'account' | 'free') {
  await act(async () => renderer.root.findByProps({ id: `router-tab-${tab}` }).props.onClick());
}
async function openAccountTab() {
  await open();
  await selectTab('account');
}
function checkbox() {
  return renderer.root.findByProps({ type: 'checkbox' });
}
function plus() {
  return renderer.root.findByProps({ 'aria-label': 'freeModels.customAdd' });
}
function rows() {
  return renderer.root
    .findAllByType('button')
    .filter((node) => String(node.props['aria-label']).startsWith('freeModels.addToRouter: '));
}
async function acknowledge(checked: boolean) {
  await act(async () => checkbox().props.onChange({ target: { checked } }));
}
function expectLocked() {
  expect(checkbox().props.checked).toBe(false);
  expect(rows()).toHaveLength(accountApi.accountProxyProviders.length);
  expect(rows().every((row) => row.props.disabled === true)).toBe(true);
  expect(plus().props.disabled).toBe(true);
}
function expectUnlocked() {
  expect(checkbox().props.checked).toBe(true);
  expect(rows()).toHaveLength(accountApi.accountProxyProviders.length);
  expect(rows().every((row) => !row.props.disabled)).toBe(true);
  expect(plus().props.disabled).toBeFalsy();
}
async function remount() {
  act(() => renderer.unmount());
  await openAccountTab();
}
function expectPassive() {
  expect(accountApi.getAccountProxyModels).not.toHaveBeenCalled();
  expect(api.getFreeModelDirectory).not.toHaveBeenCalled();
  expect(api.setSmartRouterEnabled).not.toHaveBeenCalled();
  expect(api.setSmartRouterCandidates).not.toHaveBeenCalled();
  expect(api.removeSmartRouterCandidate).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
  expect(openExternal).not.toHaveBeenCalled();
  expect(fetch).not.toHaveBeenCalled();
  expect(showToast).not.toHaveBeenCalled();
}

beforeEach(async () => {
  language.locale = 'zh-Hans';
  await loadLocale(language.locale);
  stored = new Map();
  getItem = vi.fn((key: string) => stored.get(key) ?? null);
  setItem = vi.fn((key: string, value: string) => stored.set(key, value));
  fetch = vi.fn().mockRejectedValue(new Error('Unexpected network request'));
  vi.stubGlobal('localStorage', { getItem, setItem });
  vi.stubGlobal('fetch', fetch);
  useNavigationStore.setState({ activePage: 'freeModels' });
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
  vi.mocked(accountApi.listProxyAccounts).mockResolvedValue([]);
  confirm.mockResolvedValue(true);
  invoke.mockRejectedValue(new Error('Unexpected native operation'));
});
afterEach(() => {
  act(() => renderer?.unmount());
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

it('locks both account entry points for missing, damaged or unreadable acknowledgement without rewriting storage', async () => {
  for (const value of [null, 'false', 'TRUE', '1', 'damaged']) {
    stored.clear();
    if (value !== null) stored.set(storageKey, value);
    await openAccountTab();
    expectLocked();
    await act(async () => {
      rows().forEach((row) => row.props.onClick());
      plus().props.onClick();
    });
    expect(context.setShowAddModelModal).not.toHaveBeenCalled();
    act(() => renderer.unmount());
  }
  getItem.mockImplementation(() => {
    throw new Error('Storage read unavailable');
  });
  await openAccountTab();
  expectLocked();
  expect(setItem).not.toHaveBeenCalled();
  expect(confirm).not.toHaveBeenCalled();
  expectPassive();
});

it('opens both account entry points only after acknowledgement and remembers it across tabs, pages and remounts', async () => {
  await openAccountTab();
  await acknowledge(true);
  expectUnlocked();
  expect(setItem).toHaveBeenCalledExactlyOnceWith(storageKey, 'true');
  expect(stored.get(storageKey)).toBe('true');
  act(() => rows()[0].props.onClick());
  expect(context.setNewModelForm).toHaveBeenLastCalledWith(
    expect.objectContaining({ accountSource: { provider: 'claudeCode', accountId: '' } })
  );
  act(() => plus().props.onClick());
  expect(context.setNewModelForm).toHaveBeenLastCalledWith(
    expect.objectContaining({ accountSource: { provider: 'codex', accountId: '' } })
  );
  expect(context.setShowAddModelModal).toHaveBeenCalledTimes(2);
  await selectTab('saved');
  await selectTab('account');
  expectUnlocked();
  await act(async () => useNavigationStore.getState().setActivePage('accounts'));
  await act(async () => useNavigationStore.getState().setActivePage('freeModels'));
  expectUnlocked();
  await remount();
  expectUnlocked();
  expect(setItem).toHaveBeenCalledOnce();
  expectPassive();
});

it('revokes remembered acknowledgement immediately and remains locked after remounting', async () => {
  stored.set(storageKey, 'true');
  await openAccountTab();
  expectUnlocked();
  await acknowledge(false);
  expectLocked();
  expect(setItem).toHaveBeenCalledExactlyOnceWith(storageKey, 'false');
  act(() => {
    rows()[0].props.onClick();
    plus().props.onClick();
  });
  expect(context.setShowAddModelModal).not.toHaveBeenCalled();
  await remount();
  expectLocked();
  expectPassive();
});

it.each(['en', 'ja', 'zh-Hans', 'zh-Hant'])(
  'shows the %s risk notice without treating dismissal as acknowledgement or opening an account form',
  async (locale) => {
    language.locale = locale;
    await loadLocale(locale);
    await openAccountTab();
    const notice = renderer.root
      .findAllByType('button')
      .find((node) => node.props['aria-haspopup'] === 'dialog')!;
    expect(notice.children).toEqual([translate('accountProxy.riskNotice', locale)]);
    expect(notice.props['aria-haspopup']).toBe('dialog');
    expect(notice.props.className).toContain('underline');
    expect(checkbox().props.className).toBe('peer sr-only');
    expect(checkbox().props['aria-label']).toBe(
      translate('accountProxy.riskReadPrefix', locale) +
        translate('accountProxy.riskNotice', locale) +
        translate('accountProxy.riskAcceptSuffix', locale)
    );
    const label = checkbox().parent!;
    expect(label.findAllByType('span')[0].props.className).toContain('peer-focus-visible:ring-2');
    const textLabels = notice.parent!.findAllByType('label');
    expect(textLabels.map((node) => node.children.join(''))).toEqual([
      translate('accountProxy.riskReadPrefix', locale),
      translate('accountProxy.riskAcceptSuffix', locale),
    ]);
    expect(textLabels.every((node) => node.props.htmlFor === checkbox().props.id)).toBe(true);
    expect(notice.parent!.findAllByType('button')).toEqual([notice]);
    expect(notice.parent!.parent!.children[1]).toBe(label);
    await act(async () => notice.props.onClick());
    expect(confirm).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({
        title: translate('accountProxy.riskTitle', locale),
        type: 'warning',
        cancelText: '',
        confirmText: translate('accountProxy.riskDismiss', locale),
        message: translate('accountProxy.riskMessage', locale),
      })
    );
    expectLocked();
    expect(setItem).not.toHaveBeenCalled();
    expect(context.setShowAddModelModal).not.toHaveBeenCalled();
    expectPassive();
  }
);

it('keeps ordinary model additions available while account entries remain locked', async () => {
  await open();
  for (const tab of ['saved', 'free'] as const) {
    await selectTab(tab);
    expect(plus().props.disabled).toBeFalsy();
    act(() => plus().props.onClick());
    expect(context.setNewModelForm).toHaveBeenLastCalledWith({
      name: '',
      baseUrl: '',
      anthropicUrl: '',
      apiKey: '',
      modelId: '',
    });
  }
  expect(context.setShowAddModelModal).toHaveBeenCalledTimes(2);
  await selectTab('account');
  expectLocked();
  expect(setItem).not.toHaveBeenCalled();
  expectPassive();
});

it('keeps checkbox state usable when storage writes fail without silently persisting consent', async () => {
  setItem.mockImplementation(() => {
    throw new Error('Storage write unavailable');
  });
  await openAccountTab();
  await acknowledge(true);
  expectUnlocked();
  act(() => plus().props.onClick());
  expect(context.setShowAddModelModal).toHaveBeenCalledOnce();
  await acknowledge(false);
  expectLocked();
  expect(stored.has(storageKey)).toBe(false);
  expect(setItem.mock.calls).toEqual([
    [storageKey, 'true'],
    [storageKey, 'false'],
  ]);
  await remount();
  expectLocked();
  expectPassive();
});
