import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { describe, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import type { LocalTool, ModelConfig } from '../../api/types';
import type { TKey } from '../../i18n';
import { ModelListCard } from '../../components/ModelListCard';
import vscodeDefinition from '../../../tools/vscode/paths.json';
import qoderDefinition from '../../../tools/qoder/paths.json';
import qoderCnDefinition from '../../../tools/qodercn/paths.json';
import claudeScienceDefinition from '../../../tools/claudescience/paths.json';
import { useNavigationStore } from '../../stores/navigationStore';
import { AppManagerContext, type AppManagerContextType } from './context';
import { AccountSectionRow } from './AccountSectionPrimitives';

vi.mock('../../components', () => ({
  EffortPulse: () => null,
  getModelIcon: () => null,
}));

vi.mock('../../utils/platform', () => ({ IS_WINDOWS: false, IS_LINUX: true, IS_MACOS: false }));

const tool: LocalTool = {
  id: 'test-tool',
  name: 'Test Tool',
  category: 'Desktop',
  installed: true,
  apiProtocol: ['openai'],
};

const models: ModelConfig[] = [
  {
    internalId: 'cloud-model',
    name: 'Cloud Model',
    modelId: 'deepseek-flash',
    baseUrl: 'https://cloud.example/v1',
    apiKey: '',
  },
  {
    internalId: 'local-server',
    name: 'Local Model',
    modelId: 'local-model-id',
    baseUrl: 'http://127.0.0.1:1234/v1',
    apiKey: '',
  },
  {
    internalId: 'smart-router',
    name: 'Auto Router',
    baseUrl: 'http://127.0.0.1:53683/v1',
    apiKey: '',
  },
];

const labels: Partial<Record<TKey, string>> = {
  'agent.badge.smart': '智能',
  'agent.badge.local': '本地',
};

describe('ModelListSection', () => {
  it.each([
    ['qoder', qoderDefinition],
    ['qodercn', qoderCnDefinition],
  ])('shows configured API models for %s', async (toolId, definition) => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { ModelListSection } = await import('./AppManagerComponents');
    expect(definition.noModelConfig).toBe(false);
    const markup = renderToStaticMarkup(
      <ModelListSection
        selectedToolData={{ ...tool, ...definition, id: toolId }}
        userModels={models}
        toolModelConfig={{}}
        selectedTool={toolId}
        handleSelectModel={vi.fn()}
        t={(key) => key}
      />
    );
    expect(markup).toContain('Cloud Model');
    expect(markup).toContain('deepseek-flash');
  });
  it('shows Anthropic models and the official login for Claude Science', async () => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { ModelListSection } = await import('./AppManagerComponents');
    expect(claudeScienceDefinition.noModelConfig).toBe(false);
    const markup = renderToStaticMarkup(
      <ModelListSection
        selectedToolData={{ ...tool, ...claudeScienceDefinition, id: 'claudescience' }}
        userModels={[{ ...models[0], anthropicUrl: 'https://cloud.example/anthropic' }]}
        toolModelConfig={{}}
        selectedTool="claudescience"
        handleSelectModel={vi.fn()}
        t={(key) => key}
      />
    );
    expect(markup).toContain('Cloud Model');
    expect(markup).toContain('Anthropic Official');
  });
  it.each(['test-tool', 'codex', 'chatgptdesktop'])(
    'hides the router for %s when smart routing is turned off',
    async (toolId) => {
      vi.stubGlobal('__APP_EDITION__', 'full');
      const { ModelListSection } = await import('./AppManagerComponents');
      const markup = renderToStaticMarkup(
        <ModelListSection
          smartRouterEnabled={false}
          selectedToolData={{ ...tool, id: toolId }}
          userModels={models}
          toolModelConfig={{ [toolId]: 'smart-router' }}
          selectedTool={toolId}
          handleSelectModel={vi.fn()}
          t={(key) => key}
        />
      );
      expect(markup).not.toContain('Auto Router');
      expect(markup).not.toContain('127.0.0.1:53683');
      expect(markup).toContain('Cloud Model');
    }
  );
  it('renders smart, local, and cloud models as one ordered list with compact badges', async () => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { ModelListSection } = await import('./AppManagerComponents');
    const markup = renderToStaticMarkup(
      <ModelListSection
        selectedToolData={tool}
        userModels={models}
        toolModelConfig={{}}
        selectedTool={tool.id}
        handleSelectModel={() => undefined}
        t={(key) => labels[key] ?? key}
      />
    );

    expect(markup).toContain('智能');
    expect(markup).toContain('本地');
    expect(markup.indexOf('Auto Router')).toBeLessThan(markup.indexOf('Local Model'));
    expect(markup.indexOf('Local Model')).toBeLessThan(markup.indexOf('Cloud Model'));
  });

  it('shows the model ID and usage instead of the API URL without extra tooltips', async () => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { ModelListSection } = await import('./AppManagerComponents');
    const markup = renderToStaticMarkup(
      <ModelListSection
        selectedToolData={{ ...tool, apiProtocol: ['openai', 'anthropic'] }}
        userModels={[{ ...models[0], anthropicUrl: 'https://cloud.example/anthropic' }]}
        toolModelConfig={{}}
        selectedTool={tool.id}
        handleSelectModel={() => undefined}
        modelUsageData={{
          'cloud-model': {
            quotas: [{ percentage: 80, resetAt: Date.now() + 60_000 }],
          },
        }}
        refreshingUsageIds={new Set()}
        onRefreshUsage={() => undefined}
        onEditModel={() => undefined}
        onDeleteModel={() => undefined}
        t={(key) => labels[key] ?? key}
      />
    );

    expect(markup).toContain('80%');
    expect(markup).toContain('deepseek-flash');
    expect(markup).not.toContain('cloud.example');
    expect(markup).not.toContain('OAI');
    expect(markup).not.toContain('ANT');
    expect(markup).not.toContain('⇄');
    expect(markup).not.toContain('OpenAI');
    expect(markup).not.toContain('Anthropic');
    expect(markup).not.toContain('title=');
    expect(markup).not.toContain('disabled=');
  });

  it('keeps router and official addresses while local and cloud models show IDs', async () => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { ModelListSection } = await import('./AppManagerComponents');
    const markup = renderToStaticMarkup(
      <ModelListSection
        selectedToolData={{ ...tool, id: 'claudedesktop' }}
        userModels={models}
        toolModelConfig={{}}
        selectedTool="claudedesktop"
        handleSelectModel={vi.fn()}
        t={(key) => key}
      />
    );
    expect(markup).toContain('127.0.0.1:53683/v1');
    expect(markup).toContain('api.anthropic.com');
    expect(markup).toContain('local-model-id');
    expect(markup).toContain('deepseek-flash');
    expect(markup).not.toContain('127.0.0.1:1234');
    expect(markup).not.toContain('cloud.example');
  });

  it.each(['codex', 'chatgptdesktop'])(
    'shows the Responses-capable router and hides the Chat-only local server from %s',
    async (toolId) => {
      vi.stubGlobal('__APP_EDITION__', 'full');
      const { ModelListSection } = await import('./AppManagerComponents');
      const markup = renderToStaticMarkup(
        <ModelListSection
          selectedToolData={{ ...tool, id: toolId }}
          userModels={models}
          toolModelConfig={{}}
          selectedTool={toolId}
          handleSelectModel={() => undefined}
          t={(key) => labels[key] ?? key}
        />
      );

      expect(markup).toContain('Auto Router');
      expect(markup).toContain('127.0.0.1:53683/v1');
      expect(markup).not.toContain('Local Model');
      expect(markup).not.toContain('OpenAI Official');
      expect(markup).toContain('Cloud Model');
    }
  );

  it.each(['codex', 'chatgptdesktop'])(
    'selects the exact router card for %s without applying configuration',
    async (toolId) => {
      vi.stubGlobal('__APP_EDITION__', 'full');
      const { ModelListSection } = await import('./AppManagerComponents');
      const selectModel = vi.fn();
      const applyModel = vi.spyOn(api, 'applyModelToTool');
      const router = { ...models[2], modelId: 'auto' };
      let renderer: ReactTestRenderer | undefined;
      try {
        act(() => {
          renderer = create(
            <ModelListSection
              selectedToolData={{ ...tool, id: toolId }}
              userModels={[router]}
              toolModelConfig={{}}
              selectedTool={toolId}
              handleSelectModel={selectModel}
              t={(key) => key}
            />
          );
        });
        expect(selectModel).not.toHaveBeenCalled();
        expect(applyModel).not.toHaveBeenCalled();
        const card = renderer!.root.findByType(ModelListCard);
        expect(card.props.model).toEqual(router);
        expect(card.props.subtitle).toBe('127.0.0.1:53683/v1');
        act(() => card.findByType('button').props.onClick());
        expect(selectModel).toHaveBeenCalledExactlyOnceWith(toolId, 'smart-router');
        expect(applyModel).not.toHaveBeenCalled();
      } finally {
        act(() => renderer?.unmount());
        applyModel.mockRestore();
      }
    }
  );

  it('hides the Grok official card while keeping user models', async () => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { ModelListSection } = await import('./AppManagerComponents');
    const markup = renderToStaticMarkup(
      <ModelListSection
        selectedToolData={{ ...tool, id: 'grok' }}
        userModels={models}
        toolModelConfig={{}}
        selectedTool="grok"
        handleSelectModel={vi.fn()}
        t={(key) => key}
      />
    );

    expect(markup).not.toContain('xAI Official');
    expect(markup).not.toContain('api.x.ai');
    expect(markup).toContain('Cloud Model');
    expect(markup).toContain('Local Model');
  });
});

describe('VS Code model panel', () => {
  it.each([false, true])('shows compatible models only when installed: %s', async (installed) => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { AppManagerPanel, AppManagerBottom } = await import('./AppManagerComponents');
    const { ModelNexusContext } = await import('../ModelNexus/context');
    const { ConfirmDialogProvider } = await import('../../components/ConfirmDialog');
    useNavigationStore.setState({ activePage: 'apps' });
    const context = {
      selectedTool: 'vscode',
      selectedToolData: { ...vscodeDefinition, id: 'vscode', installed },
      smartRouterEnabled: true,
      userModels: [
        ...models,
        {
          internalId: 'anthropic-model',
          name: 'Anthropic Model',
          modelId: 'anthropic-model-id',
          baseUrl: '',
          anthropicUrl: 'https://example.test/anthropic',
          apiKey: '',
        },
      ],
      toolModelConfig: { vscode: 'cloud-model' },
      handleSelectModel: vi.fn(),
      viewMode: 'desktop',
      launchAfterApply: false,
      agreedConfigPolicy: true,
      isLaunching: false,
    } as unknown as AppManagerContextType;
    const markup = renderToStaticMarkup(
      <ConfirmDialogProvider>
        <ModelNexusContext.Provider value={{} as React.ContextType<typeof ModelNexusContext>}>
          <AppManagerContext.Provider value={context}>
            <AppManagerPanel />
            <AppManagerBottom />
          </AppManagerContext.Provider>
        </ModelNexusContext.Provider>
      </ConfirmDialogProvider>
    );
    for (const name of ['Cloud Model', 'Local Model', 'Auto Router', 'Anthropic Model']) {
      expect(markup.includes(name)).toBe(installed);
    }
    expect(markup).not.toContain('agent.noModelConfig');
    expect(markup).not.toContain('agent.addAccount');
    expect(markup).not.toContain('role="switch"');
    expect(markup).not.toContain('agent.restore');
    expect(markup).toContain(installed ? 'btn.modifyOnly' : 'btn.installOneClick');
    expect(context.handleSelectModel).not.toHaveBeenCalled();
  });
});

describe('CodexAccountSection', () => {
  it('renders saved accounts with pending selection and current-state metadata', async () => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { CodexAccountSection } = await import('./AppManagerComponents');
    const context: Partial<AppManagerContextType> = {
      codexAccounts: [
        {
          id: 'account-1',
          email: 'first@example.com',
          plan: 'prolite',
          subscriptionEndAt: 1_800_086_400,
          quotaWindows: [
            { label: '5h', remainingPercent: 65, resetAt: 1_800_000_000 },
            { label: '7d', remainingPercent: 32, resetAt: 1_800_086_400 },
          ],
          quotaPercent: 32,
          quotaResetAt: 1_800_000_000,
          active: true,
        },
      ],
      selectedCodexAccountId: 'account-1',
      setSelectedCodexAccountId: () => {},
      isLoadingCodexAccounts: false,
      isAddingCodexAccount: false,
      codexOAuthRemainingSeconds: 0,
      refreshingCodexAccountIds: new Set(),
      codexAuthorizationFailedIds: new Set(),
      addCodexAccount: async () => {},
      refreshCodexAccountQuota: async () => undefined,
      deleteCodexAccount: async () => {},
    };

    const markup = renderToStaticMarkup(
      <AppManagerContext.Provider value={context as AppManagerContextType}>
        <CodexAccountSection showDivider={false} />
      </AppManagerContext.Provider>
    );

    expect(markup).toContain('first@example.com');
    expect(markup).toContain('Pro 100');
    expect(markup).toContain('65%');
    expect(markup).toContain('32%');
    expect(markup).not.toContain('5h 65%');
    expect(markup).not.toContain('7d 32%');
    expect(markup).not.toContain('bg-cyber-border');
    expect(markup).toContain('aria-checked="true"');
    expect(markup).toContain('agent.refreshAccount');
    expect(markup).not.toContain('role="tooltip"');
    expect(markup).not.toContain('border-b');
  });

  it('uses the progress bar for one quota window and puts the subscription before the plan', async () => {
    const { CodexAccountSection } = await import('./AppManagerComponents');
    const context: Partial<AppManagerContextType> = {
      codexAccounts: [
        {
          id: 'account-1',
          email: 'a@example.com',
          plan: 'prolite',
          subscriptionEndAt: 1_800_086_400,
          quotaWindows: [{ label: '7d', remainingPercent: 31, resetAt: 1_800_000_000 }],
          active: false,
        },
      ],
      selectedCodexAccountId: null,
      isLoadingCodexAccounts: false,
      isAddingCodexAccount: false,
      codexOAuthRemainingSeconds: 0,
      refreshingCodexAccountIds: new Set(),
      codexAuthorizationFailedIds: new Set(),
    };
    const markup = renderToStaticMarkup(
      <AppManagerContext.Provider value={context as AppManagerContextType}>
        <CodexAccountSection showDivider={false} />
      </AppManagerContext.Provider>
    );
    expect(markup).toContain('bg-cyber-border');
    expect(markup).toContain('31%');
    expect(markup).not.toContain('7d 31%');
    const row = renderToStaticMarkup(
      <AccountSectionRow
        selected={false}
        email="a@example.com"
        plan="Pro 100"
        planPrefix={<span>remaining</span>}
        onSelect={() => {}}
        onDelete={() => {}}
      />
    );
    expect(row.indexOf('remaining')).toBeLessThan(row.indexOf('Pro 100'));
  });

  it.each([
    ['free', 'Free'],
    ['go', 'Go'],
    ['plus', 'Plus'],
    ['pro', 'Pro 200'],
    ['prolite', 'Pro 100'],
    ['pro-5x', 'Pro 100'],
    ['pro-20x', 'Pro 200'],
    ['promax', 'Pro 500'],
    ['pro_100', 'Pro 100'],
    ['pro_200', 'Pro 200'],
    ['pro_500', 'Pro 500'],
    ['team', 'Business'],
    ['enterprise', 'Enterprise'],
  ])('labels %s as %s', async (plan, label) => {
    const { CodexAccountSection } = await import('./AppManagerComponents');
    const context: Partial<AppManagerContextType> = {
      codexAccounts: [{ id: 'account-1', email: 'a@example.com', plan, active: false }],
      selectedCodexAccountId: null,
      isLoadingCodexAccounts: false,
      isAddingCodexAccount: false,
      codexOAuthRemainingSeconds: 0,
      refreshingCodexAccountIds: new Set(),
      codexAuthorizationFailedIds: new Set(),
    };
    const markup = renderToStaticMarkup(
      <AppManagerContext.Provider value={context as AppManagerContextType}>
        <CodexAccountSection showDivider={false} />
      </AppManagerContext.Provider>
    );
    expect(markup).toContain(`>${label}</span>`);
  });

  it('reserves enough room for a long plan without a subscription countdown', async () => {
    const { CodexAccountSection } = await import('./AppManagerComponents');
    const context: Partial<AppManagerContextType> = {
      codexAccounts: [
        { id: 'account-1', email: 'long@example.com', plan: 'enterprise', active: false },
      ],
      selectedCodexAccountId: null,
      isLoadingCodexAccounts: false,
      isAddingCodexAccount: false,
      codexOAuthRemainingSeconds: 0,
      refreshingCodexAccountIds: new Set(),
      codexAuthorizationFailedIds: new Set(),
    };
    const markup = renderToStaticMarkup(
      <AppManagerContext.Provider value={context as AppManagerContextType}>
        <CodexAccountSection showDivider={false} />
      </AppManagerContext.Provider>
    );
    expect(markup).toContain('grid-cols-[16px_minmax(0,1fr)_72px]');
    expect(markup).toContain('>Enterprise</span>');
  });
});

describe('AppManager views', () => {
  const installedTool = { ...tool, id: 'installed-app', name: 'Installed App' };
  const uninstalledTool = {
    ...tool,
    id: 'uninstalled-app',
    name: 'Uninstalled App',
    installed: false,
  };

  const renderView = async (viewMode: 'desktop' | 'install', detectedTools: LocalTool[]) => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    const { AppManagerMain } = await import('./AppManagerComponents');
    const context: Partial<AppManagerContextType> = {
      detectedTools,
      viewMode,
      isScanning: false,
      selectedTool: null,
      setSelectedTool: () => {},
      aiInstallableIds: ['uninstalled-app'],
    };
    return renderToStaticMarkup(
      <AppManagerContext.Provider value={context as AppManagerContextType}>
        <AppManagerMain />
      </AppManagerContext.Provider>
    );
  };

  it.each([
    ['desktop', 'Installed App', 'Uninstalled App'],
    ['install', 'Uninstalled App', 'Installed App'],
  ] as const)('%s shows only the matching apps', async (mode, visible, hidden) => {
    const markup = await renderView(mode, [installedTool, uninstalledTool]);
    expect(markup).toContain(`aria-label="${visible}"`);
    expect(markup).not.toContain(`aria-label="${hidden}"`);
  });

  it.each([
    ['desktop', [uninstalledTool], 'aiDesktop.emptyDesktop'],
    ['install', [installedTool], 'aiDesktop.emptyInstall'],
  ] as const)('%s explains an empty list', async (mode, tools, message) => {
    const markup = await renderView(mode, [...tools]);
    expect(markup).toContain(message);
  });
});

describe('PageAwareHint', () => {
  const renderHint = async (selectedTool: string | null, selectedAccount: string | null = null) => {
    vi.stubGlobal('__APP_EDITION__', 'full');
    useNavigationStore.setState({ activePage: 'apps' });
    const { PageAwareHint } = await import('./AppManagerComponents');
    const context: Partial<AppManagerContextType> = {
      viewMode: 'desktop',
      selectedTool,
      claudeCodeAccounts: {
        selectedId: selectedAccount,
      } as AppManagerContextType['claudeCodeAccounts'],
      claudeDesktopAccounts: {
        selectedId: selectedAccount,
      } as AppManagerContextType['claudeDesktopAccounts'],
    };
    return renderToStaticMarkup(
      <AppManagerContext.Provider value={context as AppManagerContextType}>
        <PageAwareHint />
      </AppManagerContext.Provider>
    );
  };

  it.each(['claudedesktop', 'claudecode'])(
    'shows the keep-running reminder only for %s',
    async (toolId) => {
      const markup = await renderHint(toolId);
      expect(markup).toContain('hint.devInvite');
      expect(markup).not.toContain('hint.responsesRequired');
    }
  );

  it.each(['chatgptdesktop', 'codex'])(
    'shows the Responses compatibility reminder for %s',
    async (toolId) => {
      const markup = await renderHint(toolId);
      expect(markup).toContain('hint.responsesRequired');
      expect(markup).not.toContain('hint.devInvite');
    }
  );

  it('hides the proxy reminder when a Claude Code account is selected', async () => {
    const markup = await renderHint('claudecode', 'saved-account');
    expect(markup).not.toContain('hint.devInvite');
  });
  it('hides the proxy reminder when a Claude Desktop account is selected', async () => {
    expect(await renderHint('claudedesktop', 'saved-account')).not.toContain('hint.devInvite');
  });

  it('shows neither tool-specific reminder for other tools', async () => {
    const markup = await renderHint('test-tool');
    expect(markup).not.toContain('hint.devInvite');
    expect(markup).not.toContain('hint.responsesRequired');
  });
});

describe.each(['codex', 'chatgptdesktop'])('%s Web Search control', (client) => {
  it.each([
    { installed: true, account: null, pending: false, hasModels: true },
    { installed: true, account: null, pending: true, hasModels: true },
    { installed: true, account: 'account', pending: false, hasModels: false },
    { installed: false, account: null, pending: false, hasModels: false },
  ])(
    'restores visibility for the selected tool and disables pending changes: %j',
    async ({ installed, account, pending, hasModels }) => {
      const { AppManagerPanel } = await import('./AppManagerComponents');
      const { ModelNexusContext } = await import('../ModelNexus/context');
      const { ConfirmDialogProvider } = await import('../../components/ConfirmDialog');
      const context = {
        selectedTool: client,
        selectedToolData: { ...tool, id: client, installed },
        userModels: hasModels ? models : [],
        toolModelConfig: {},
        codexAccounts: [],
        selectedCodexAccountId: account,
        refreshingCodexAccountIds: new Set(),
        codexAuthorizationFailedIds: new Set(),
        codexWebSearch: false,
        isLaunching: pending,
      } as unknown as AppManagerContextType;
      const markup = renderToStaticMarkup(
        <ConfirmDialogProvider>
          <ModelNexusContext.Provider value={{} as React.ContextType<typeof ModelNexusContext>}>
            <AppManagerContext.Provider value={context}>
              <AppManagerPanel />
            </AppManagerContext.Provider>
          </ModelNexusContext.Provider>
        </ConfirmDialogProvider>
      );
      const toggle = markup.match(/<button[^>]*role="switch"[^>]*>/)?.[0];
      if (installed) {
        expect(toggle).toContain('aria-label="agent.codexWebSearchLabel"');
        expect(toggle).toContain('aria-checked="false"');
        expect(toggle?.includes('disabled=""')).toBe(pending);
        expect(markup).toContain('aria-label="agent.codexWebSearchHint"');
        expect(markup.indexOf('agent.modelSwitch')).toBeLessThan(markup.indexOf('role="switch"'));
        if (hasModels)
          expect(markup.indexOf('role="switch"')).toBeLessThan(markup.indexOf('Cloud Model'));
      } else {
        expect(toggle).toBeUndefined();
      }
      expect(markup).not.toContain('agent.codexRelayLabel');
      expect(markup).not.toContain('role="tooltip"');
      expect(markup).not.toContain('cursor-help');
    }
  );
});

describe.each(['claudedesktop', 'claudecode'])('%s routing controls', (client) => {
  it.each([
    { relay: false, selectedAccount: null },
    { relay: true, selectedAccount: null },
    { relay: false, selectedAccount: 'saved-account' },
    { relay: true, selectedAccount: 'saved-account' },
  ])(
    'keeps API Router visible regardless of account selection: %j',
    async ({ relay, selectedAccount }) => {
      const { AppManagerPanel } = await import('./AppManagerComponents');
      const { ModelNexusContext } = await import('../ModelNexus/context');
      const { ConfirmDialogProvider } = await import('../../components/ConfirmDialog');
      const accounts = {
        accounts: selectedAccount
          ? [
              {
                id: selectedAccount,
                email: 'saved@example.test',
                plan: null,
                fiveHour: null,
                sevenDay: null,
              },
            ]
          : [],
        selectedId: selectedAccount,
        busy: false,
        remainingSeconds: 0,
        refreshing: new Set(),
        authorizationFailedIds: new Set(),
        add: async () => {},
        select: () => {},
        refresh: async () => {},
        remove: async () => {},
      };
      const context = {
        selectedTool: client,
        selectedToolData: { ...tool, id: client },
        toolModelConfig: {},
        userModels: [],
        claudeDesktopRelayMode: relay,
        claudeCodeRelayMode: relay,
        claudeDesktop1mMode: true,
        claude1mMode: true,
        claudeCodeAccounts: accounts,
        claudeDesktopAccounts: accounts,
      } as unknown as AppManagerContextType;
      const markup = renderToStaticMarkup(
        <ConfirmDialogProvider>
          <ModelNexusContext.Provider value={{} as React.ContextType<typeof ModelNexusContext>}>
            <AppManagerContext.Provider value={context}>
              <AppManagerPanel />
            </AppManagerContext.Provider>
          </ModelNexusContext.Provider>
        </ConfirmDialogProvider>
      );
      expect(markup).toContain(`aria-checked="${relay}" aria-label="agent.codexRelayLabel"`);
      if (client === 'claudedesktop' || relay) {
        expect(markup).toMatch(/aria-checked="true" aria-label="1M"/);
        expect(markup).toContain('agent.claude1mHint');
      } else {
        expect(markup).not.toContain('aria-label="1M"');
      }
      if (client === 'claudedesktop') expect(markup).toContain('Anthropic Official');
      if (selectedAccount) expect(markup).toContain('<div role="radio" aria-checked="true"');
    }
  );
});

describe.each(['grokbot', 'cursor', 'manus', 'cue'] as const)('%s account-only panel', (client) => {
  it('shows accounts for a no-model-config tool without rendering API models', async () => {
    const { AppManagerPanel, AppManagerBottom } = await import('./AppManagerComponents');
    const { ModelNexusContext } = await import('../ModelNexus/context');
    const { ConfirmDialogProvider } = await import('../../components/ConfirmDialog');
    const context = {
      selectedTool: client,
      selectedToolData: { ...tool, id: client, name: client, noModelConfig: true },
      userModels: models,
      toolModelConfig: { [client]: 'cloud-model' },
      [client === 'cursor'
        ? 'cursorAccounts'
        : client === 'manus'
          ? 'manusAccounts'
          : client === 'cue'
            ? 'cueAccounts'
            : 'grokBotAccounts']: {
        accounts: [
          {
            id: 'one',
            email: 'bot@example.test',
            active: true,
            credits: client === 'manus' || client === 'cue' ? { total: 1300 } : undefined,
          },
        ],
        selectedId: 'one',
        busy: true,
        remainingSeconds: 60,
        refreshing: new Set(),
        authorizationFailedIds: new Set(),
      },
      viewMode: 'desktop',
      launchAfterApply: false,
      isLaunching: false,
    } as unknown as AppManagerContextType;
    const markup = renderToStaticMarkup(
      <ConfirmDialogProvider>
        <ModelNexusContext.Provider value={{} as React.ContextType<typeof ModelNexusContext>}>
          <AppManagerContext.Provider value={context}>
            <AppManagerPanel />
            <AppManagerBottom />
          </AppManagerContext.Provider>
        </ModelNexusContext.Provider>
      </ConfirmDialogProvider>
    );
    expect(markup).toContain('bot@example.test');
    if (client === 'manus' || client === 'cue') expect(markup).toContain('1300 agent.credits');
    expect(markup).not.toContain('Cloud Model');
    expect(markup).not.toContain('Auto Router');
    expect(markup).not.toContain('agent.noModelConfig');
    expect(markup).toContain('btn.launchApp');
    expect(markup.match(/<button[^>]*disabled=""/g)).toHaveLength(1);
    expect(markup).toContain('aria-label="btn.cancel"');
  });
});
