// App.tsx — Tauri application shell (lightweight router)
// Layout matches the original v1.1.0 structure exactly.
// Pages extracted to src/pages/ with Provider pattern.
// All Providers are always mounted; pages are shown/hidden via CSS to avoid remounting.

import { useState, useEffect } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { listen } from '@tauri-apps/api/event';
import { IS_MACOS } from './utils/platform';
import { Sidebar, PageType, ToastProvider, ConfirmDialogProvider } from './components';
import { isNewerVersion } from './utils/version';
import { DownloadProvider } from './components/DownloadContext';
import { DownloadBar } from './components/DownloadBar';
import { TitleBar } from './components/TitleBar';
import { SettingsDialog } from './components/SettingsDialog';
import { ChangelogDialog } from './components/ChangelogDialog';
import { getSettings } from './api/tauri';

import { useI18n } from './hooks/useI18n';

// Zustand stores
import { useToolsStore } from './stores/toolsStore';
import { useNavigationStore } from './stores/navigationStore';

// Pages
import {
  ModelNexusProvider,
  ModelNexusTitleActions,
  ModelNexusMain,
  ModelNexusPanel,
  AddModelModal,
} from './pages/ModelNexus';

import {
  AppManagerProvider,
  AppManagerMain,
  AppManagerTitleActions,
  AppManagerPanel,
  AppManagerBottom,
  AppManagerErrorModal,
} from './pages/AppManager';
import {
  LocalServerProvider,
  LocalServerMain,
  LocalServerPanel,
  LocalServerBottom,
} from './pages/LocalServer';
import { MotherAgentProvider, MotherAgentMain, MotherAgentPanel } from './pages/MotherAgent';
import { FeedbackMain } from './pages/Feedback';
import { MyProjectsMain, MyProjectsPanel, MyProjectsBottom } from './pages/MyProjects';
import { AiCareerMain, AiCareerFamilyPanel, AiCareerTitleActions } from './pages/AiCareer';
import { AiPulseProvider, AiPulseTitleActions, AiPulsePanel, AiNewsMain } from './pages/AiPulse';
import { useMyProjectsStore } from './stores/myProjectsStore';
import {
  AccountCenterMain,
  AccountCenterPanel,
  AccountCenterTitleActions,
} from './pages/AccountCenter/AccountCenter';
import {
  FreeModelsProvider,
  FreeModelsTitleActions,
  FreeModelsMain,
  FreeModelsPanel,
  useFreeModels,
} from './pages/FreeModels';

function SidebarConnected({ onSettingsClick }: { onSettingsClick: () => void }) {
  // Selector form (one field per call) so unrelated store fields like
  // agentRunning ticking don't re-render the sidebar — and through it,
  // the whole app tree on tab switches.
  const activePage = useNavigationStore((s) => s.activePage);
  const setActivePage = useNavigationStore((s) => s.setActivePage);
  const agentRunning = useNavigationStore((s) => s.agentRunning);
  const updateAvailable = useNavigationStore((s) => s.updateAvailable);
  const { customModels, routerEnabled, routerTogglePending, setRouterEnabled } = useFreeModels();
  return (
    <Sidebar
      activePage={activePage}
      onPageChange={setActivePage}
      agentRunning={agentRunning}
      hasSmartRouterModels={customModels.length > 0}
      smartRouterEnabled={routerEnabled}
      smartRouterTogglePending={routerTogglePending}
      onSmartRouterChange={(enabled) => void setRouterEnabled(enabled)}
      updateAvailable={updateAvailable}
      onSettingsClick={onSettingsClick}
    />
  );
}

// Helper: h (hidden) vs shown class
const page = (active: boolean) => (active ? 'contents' : 'hidden');
const pageBlock = (active: boolean) => (active ? 'flex-1 flex flex-col overflow-hidden' : 'hidden');
const pageScroll = (active: boolean) => (active ? 'flex-1 overflow-y-auto page-scroll' : 'hidden');

function AppContent() {
  const { t, locale, setLocale } = useI18n();
  const [showSettings, setShowSettings] = useState(false);
  const [showChangelog, setShowChangelog] = useState(false);

  // Stores — selector form so App.tsx (the root of the entire tree) only
  // re-renders when activePage flips, not on every motherNewMessage tick.
  const activePage = useNavigationStore((s) => s.activePage);
  const setActivePage = useNavigationStore((s) => s.setActivePage);
  const setUpdateAvailable = useNavigationStore((s) => s.setUpdateAvailable);
  const scanTools = useToolsStore((s) => s.scanTools);
  // When a user project is selected on "我的AI项目", the right panel +
  // bottom bar swap to MyProjectsPanel / MyProjectsBottom (their state is
  // mutually exclusive with AppManager.selectedTool; see myProjects'
  // handleSelect). Read from the store at the root level so the swap is a
  // single conditional render rather than per-component CSS toggles.
  const selectedUserProjectId = useMyProjectsStore((s) => s.selectedUserProjectId);
  const useMyProjectsPanel = activePage === 'myProjects' && !!selectedUserProjectId;

  // ── Post-mount work — the window is already shown (main.tsx fires
  // appReady after first paint). Scan installed tools and check for app
  // updates in the background; both are non-blocking.
  useEffect(() => {
    const preload = async () => {
      try {
        await scanTools();
      } catch {
        /* continue anyway */
      }
      // Resolve the installed version first (local, instant). The changelog
      // auto-open is a purely local decision so it still fires when offline
      // (the dialog then shows its own error state); the update check below
      // needs the network.
      const appVersion = await getVersion().catch(() => '');
      if (appVersion) {
        try {
          const seen = localStorage.getItem('echobird-changelog-seen');
          // Open on the first launch of any version we haven't recorded yet: a
          // fresh install / reinstall (no baseline) or a genuine upgrade. A real
          // uninstall wipes localStorage, so "updated" and "first install" are
          // indistinguishable — showing the notes once on either is the robust
          // choice. Only a downgrade (seen is newer) is skipped.
          if (!seen || isNewerVersion(appVersion, seen)) {
            setShowChangelog(true);
          }
          if (seen !== appVersion) {
            localStorage.setItem('echobird-changelog-seen', appVersion);
          }
        } catch {
          /* localStorage unavailable — skip the auto-open */
        }
      }
      try {
        const res = await fetch('https://echobird.ai/api/version/index.json');
        if (res.ok && appVersion) {
          const data = await res.json();
          if (data.version && isNewerVersion(data.version, appVersion)) {
            setUpdateAvailable(data.version);
          }
        }
      } catch {
        /* network error — ignore silently */
      }
    };
    preload();
    // scanTools + setUpdateAvailable are stable app-level actions; this is
    // intentionally a run-once-after-first-paint effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Track maximized state so the rounded-corner shell goes square when the
  // window fills the screen (rounded corners against screen edges look off).
  const [isMaximized, setIsMaximized] = useState(false);
  useEffect(() => {
    const win = getCurrentWindow();
    win
      .isMaximized()
      .then(setIsMaximized)
      .catch(() => {});
    const unlisten = win.onResized(() => {
      win
        .isMaximized()
        .then(setIsMaximized)
        .catch(() => {});
    });
    return () => {
      unlisten.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Wire the macOS app-menu items to the same actions as the in-window buttons:
  // Settings (⌘,) opens the Settings dialog, Feedback opens the Feedback page.
  // The Rust menu handler emits these events; on Windows/Linux there is no app
  // menu, so they simply never fire.
  useEffect(() => {
    const unlistens: Array<() => void> = [];
    listen('menu-open-settings', () => setShowSettings(true)).then((u) => unlistens.push(u));
    listen('menu-open-feedback', () => setActivePage('feedback')).then((u) => unlistens.push(u));
    return () => unlistens.forEach((u) => u());
  }, [setActivePage]);

  // Intercept window close to support "minimize to tray" behavior
  useEffect(() => {
    const win = getCurrentWindow();
    const setupCloseHandler = async () => {
      const unlisten = await win.onCloseRequested(async (event) => {
        // Check user settings
        const settings = await getSettings();
        const closeToTray = settings.closeToTray ?? false;

        if (closeToTray) {
          // Prevent default close and hide window instead
          event.preventDefault();
          await win.hide();
        }
        // Otherwise let the window close normally
      });

      return unlisten;
    };

    let unlistenFn: (() => void) | null = null;
    setupCloseHandler()
      .then((fn) => {
        unlistenFn = fn;
      })
      .catch((err) => {
        console.error('[App] Failed to setup close handler:', err);
      });

    return () => {
      if (unlistenFn) {
        unlistenFn();
      }
    };
  }, []);

  // Mirror isMaximized to <html> so the global #root clip-path in index.css
  // can drop its rounded corners when the window is maximized. Necessary
  // because the clip lives at #root level (covering modal/toast portals
  // outside the .rounded-xl shell) and CSS can't read React state directly.
  useEffect(() => {
    document.documentElement.classList.toggle('window-maximized', isMaximized);
  }, [isMaximized]);

  useEffect(() => {
    document.documentElement.classList.toggle('local-llm-page', activePage === 'localLlm');
    return () => document.documentElement.classList.remove('local-llm-page');
  }, [activePage]);

  const is = (p: PageType) => activePage === p;

  return (
    <ToastProvider>
      <FreeModelsProvider>
        <ConfirmDialogProvider>
          <DownloadProvider>
            {/* All Providers always mounted — only CSS hidden changes */}
            <MotherAgentProvider>
              <ModelNexusProvider>
                <AppManagerProvider>
                  <LocalServerProvider>
                    <div
                      className={`flex flex-col h-screen w-full bg-cyber-bg overflow-hidden ${isMaximized || IS_MACOS ? '' : 'rounded-xl'}`}
                    >
                      {/* Title bar */}
                      <TitleBar
                        onSettingsClick={() => setShowSettings(true)}
                        onFeedbackClick={() => setActivePage('feedback')}
                        onChangelogClick={() => setShowChangelog(true)}
                      />
                      <div className="flex flex-1 overflow-hidden text-cyber-text font-sans p-4 gap-0 relative isolate">
                        {/* Sidebar */}
                        <SidebarConnected onSettingsClick={() => setShowSettings(true)} />

                        {/* Main content wrapper — transparent against page bg, Claude-style */}
                        <div className="flex-1 flex flex-col overflow-hidden">
                          {/* Main + Right panel row */}
                          <div className="flex-1 flex gap-3 overflow-hidden">
                            <main className="flex-1 flex flex-col overflow-hidden">
                              <section className="flex-1 flex flex-col overflow-hidden pr-2">
                                {/* Shared page title bar — fixed-height row so the title sits at the same baseline whether the page has tall action buttons or none */}
                                <div className="mb-5 flex-shrink-0 flex items-center gap-3 h-10">
                                  <div className="flex items-baseline gap-3 flex-1 min-w-0">
                                    <h2 className="cjk-title flex-shrink-0">
                                      {is('models') && t('page.modelNexus')}
                                      {is('accounts') && t('page.accountCenter')}
                                      {is('freeModels') && t('page.freeModels')}

                                      {is('apps') && t('page.appManager')}
                                      {is('myProjects') && t('page.myProjects')}
                                      {is('aiCareer') && t('page.aiCareer')}
                                      {is('aiNews') && t('page.aiNews')}
                                      {is('localLlm') && t('page.localServer')}
                                      {is('mother') && t('page.motherAgent')}
                                      {is('feedback') && t('page.feedback')}
                                    </h2>
                                    <div className="page-kicker truncate" aria-hidden="true">
                                      {is('models') && 'MODELS'}
                                      {is('accounts') && 'ACCOUNTS'}
                                      {is('freeModels') &&
                                        (locale.startsWith('zh') ? 'ACCESS' : 'ROUTER')}
                                      {is('apps') && 'DESKTOP'}
                                      {is('myProjects') && 'VIBE CODING'}
                                      {is('aiCareer') && 'CAREER'}
                                      {is('aiNews') && 'NEWS'}
                                      {is('localLlm') && 'RUNTIME'}
                                      {is('mother') && 'AGENT'}
                                      {is('feedback') && 'SUPPORT'}
                                    </div>
                                  </div>
                                  {/* Title actions — always mounted but hidden */}
                                  <span className={page(is('models'))}>
                                    <ModelNexusTitleActions />
                                  </span>
                                  <span className={page(is('accounts'))}>
                                    <AccountCenterTitleActions />
                                  </span>
                                  <span className={page(is('freeModels'))}>
                                    <FreeModelsTitleActions />
                                  </span>
                                  <span className={page(is('aiCareer'))}>
                                    <AiCareerTitleActions />
                                  </span>
                                  <span className={page(is('aiNews'))}>
                                    <AiPulseTitleActions />
                                  </span>
                                  <span className={page(is('apps'))}>
                                    <AppManagerTitleActions />
                                  </span>
                                </div>

                                {/* Page content — always mounted, CSS hidden */}
                                <div className={pageScroll(is('models'))}>
                                  <ModelNexusMain />
                                </div>
                                <div
                                  className={
                                    is('accounts')
                                      ? 'flex-1 overflow-y-auto overflow-x-hidden cursor-default'
                                      : 'hidden'
                                  }
                                >
                                  <AccountCenterMain />
                                </div>
                                <div
                                  className={is('freeModels') ? 'flex-1 overflow-y-auto' : 'hidden'}
                                >
                                  {is('freeModels') && <FreeModelsMain />}
                                </div>

                                <div className={pageBlock(is('apps'))}>
                                  <AppManagerMain />
                                </div>
                                <div className={pageBlock(is('myProjects'))}>
                                  <MyProjectsMain />
                                </div>
                                <div className={pageBlock(is('localLlm'))}>
                                  <LocalServerMain />
                                </div>
                                <div className={pageBlock(is('aiCareer'))}>
                                  <AiCareerMain />
                                </div>
                                <div className={pageBlock(is('aiNews'))}>
                                  <AiNewsMain />
                                </div>
                                {/* MotherAgent: always mounted, hidden via CSS to preserve chat state */}
                                <div
                                  className={`flex-1 flex flex-col overflow-hidden ${is('mother') ? '' : 'hidden'}`}
                                >
                                  <MotherAgentMain />
                                </div>
                                <div className={pageScroll(is('feedback'))}>
                                  <FeedbackMain />
                                </div>
                              </section>
                            </main>

                            <aside className="w-80 flex flex-col">
                              <div className={page(is('models'))}>
                                <ModelNexusPanel />
                              </div>
                              <div className={page(is('accounts'))}>
                                <AccountCenterPanel />
                              </div>
                              <div className={page(is('freeModels'))}>
                                <FreeModelsPanel />
                              </div>

                              <div className={page(is('apps') || is('myProjects'))}>
                                {useMyProjectsPanel ? <MyProjectsPanel /> : <AppManagerPanel />}
                              </div>
                              <div className={page(is('localLlm'))}>
                                <LocalServerPanel />
                              </div>
                              <div className={page(is('aiCareer'))}>
                                <AiCareerFamilyPanel />
                              </div>
                              <div className={page(is('aiNews'))}>
                                <AiPulsePanel />
                              </div>
                              {/* MotherAgent panel: always mounted, hidden via CSS */}
                              <div className={!is('mother') ? 'hidden' : 'contents'}>
                                <MotherAgentPanel />
                              </div>
                            </aside>
                          </div>

                          {/* Bottom bars — always mounted, CSS hidden */}
                          <div className={page(is('apps') || is('myProjects'))}>
                            {useMyProjectsPanel ? <MyProjectsBottom /> : <AppManagerBottom />}
                          </div>
                          <div className={page(is('localLlm'))}>
                            <LocalServerBottom />
                          </div>

                          {/* Download bar */}
                          <div className="flex-shrink-0 pt-2">
                            <DownloadBar />
                          </div>
                        </div>
                      </div>
                    </div>

                    {/* Modals */}
                    <AddModelModal />
                    <AppManagerErrorModal />
                  </LocalServerProvider>
                </AppManagerProvider>
              </ModelNexusProvider>
            </MotherAgentProvider>

            {/* Settings dialog */}
            <SettingsDialog
              isOpen={showSettings}
              onClose={() => setShowSettings(false)}
              locale={locale}
              onLocaleChange={setLocale}
            />

            {/* Changelog dialog */}
            <ChangelogDialog isOpen={showChangelog} onClose={() => setShowChangelog(false)} />
          </DownloadProvider>
        </ConfirmDialogProvider>
      </FreeModelsProvider>
    </ToastProvider>
  );
}

function App() {
  const activePage = useNavigationStore((s) => s.activePage);
  return (
    <AiPulseProvider active={activePage === 'aiNews'}>
      <AppContent />
    </AiPulseProvider>
  );
}

export default App;
