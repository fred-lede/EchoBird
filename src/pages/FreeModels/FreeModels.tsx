import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentPropsWithRef,
  type ReactNode,
  type RefObject,
  type PointerEventHandler,
} from 'react';
import { createPortal } from 'react-dom';
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  KeyboardSensor,
  closestCenter,
  pointerWithin,
  rectIntersection,
  useDndContext,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
  type CollisionDetection,
} from '@dnd-kit/core';
import {
  SortableContext,
  useSortable,
  rectSortingStrategy,
  sortableKeyboardCoordinates,
  arrayMove,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import {
  Box,
  Check,
  CircleHelp,
  ExternalLink,
  KeyRound,
  Plus,
  RefreshCw,
  Waypoints,
  Trash2,
  X,
} from 'lucide-react';
import * as api from '../../api/tauri';
import * as modelApi from '../../api/models';
import {
  accountProxyProviders,
  accountModelPickerOptions,
  cacheAccountProxyModels,
  getAccountProxyModels,
  getCachedAccountProxyModels,
  listProxyAccounts,
  type ProxyAccount,
} from '../../api/accountProxy';
import type { AccountProvider, AccountSource, ModelConfig } from '../../api/types';
import type { FreeModelDirectory, FreeModelEntry } from '../../api/freeModels';
import { getModelIcon } from '../../components';
import { ModelListCard } from '../../components/ModelListCard';
import { ViewModeTabs } from '../../components/ViewModeTabs';
import { ModelIdCombobox } from '../../components/ModelIdCombobox';
import { useConfirm } from '../../components/ConfirmDialog';
import { useToast } from '../../components/Toast';
import { useI18n } from '../../hooks/useI18n';
import { useNavigationStore } from '../../stores/navigationStore';
import { AccountProxyPanel, openAccountModelForm } from './AccountProxyPanel';
import { copyText } from '../../utils/copyText';
import { useModelNexus } from '../ModelNexus/context';
import { accountProxyIdentityLabel, accountProxyPlanLabel } from '../AppManager/accountPlanLabel';
import './FreeModels.css';

const NODE_COLUMN_GAP = 24;
const NODE_ROW_GAP = 36;
const NODE_SIDE_PADDING = 20;
const HUB_TO_NODE_GAP = 64;
const HUB_ARROW_GAP = 4;
const HUB_ARROW_HEIGHT = 12;
const HUB_ARROW_LINE_GAP = 2;
const SCAN_STEP_MS = 230;
const SCAN_COMPLETE_MS = 320;
const ROUTER_ACTIVITY_POLL_MS = 750;
const ROUTER_ACTIVITY_GRACE_MS = 2200;
const ACCOUNT_PROXY_RISK_ACKNOWLEDGED_KEY = 'echobird-account-proxy-risk-acknowledged';
const ROUTER_REMOVE_DROP_ID = 'router-remove-drop-zone';

const routeCollisionDetection: CollisionDetection = (args) => {
  const removeTargets = args.droppableContainers.filter(
    (entry) => entry.id === ROUTER_REMOVE_DROP_ID
  );
  const removeHits = (args.pointerCoordinates ? pointerWithin : rectIntersection)({
    ...args,
    droppableContainers: removeTargets,
  });
  if (removeHits.length > 0) return removeHits;
  return closestCenter({
    ...args,
    droppableContainers: args.droppableContainers.filter(
      (entry) => entry.id !== ROUTER_REMOVE_DROP_ID
    ),
  });
};

interface FreeModelsContextValue {
  viewMode: 'smart' | 'super';
  setViewMode: (mode: 'smart' | 'super') => void;
  catalog: FreeModelDirectory;
  models: FreeModelEntry[];
  customModels: RouteModelNode[];
  proxyAccounts: Partial<Record<AccountProvider, ProxyAccount[]>>;
  selectedIds: Set<string>;
  refreshing: boolean;
  scanProvider: string;
  scanProgress: number;
  routerBaseUrl: string;
  routerEnabled: boolean;
  routerTogglePending: boolean;
  setRouterEnabled: (enabled: boolean) => Promise<void>;
  refresh: () => Promise<void>;
  addSelectedModel: (model: RouteModelInput) => Promise<void>;
  updateSelectedModel: (model: RouteModelInput) => void;
  changeSelectedModel: (id: string, modelId: string) => Promise<ModelConfig | undefined>;
  savingModelIds: Set<string>;
  removeSelectedModel: (id: string) => Promise<void>;
  reorderSelectedModel: (activeId: string, overId: string) => Promise<void>;
}

interface RouteModelInput {
  internalId: string;
  name: string;
  baseUrl: string;
  anthropicUrl?: string;
  apiKey?: string;
  modelId: string;
  accountSource?: AccountSource;
  scope?: ModelConfig['scope'];
}

interface RouteModelNode {
  id: string;
  internalId: string;
  provider: string;
  modelId: string;
  baseUrl: string;
  anthropicUrl?: string;
  apiKey?: string;
  accountSource?: AccountSource;
  scope?: ModelConfig['scope'];
}

interface FreeModelProviderGroup {
  id: string;
  name: string;
  baseUrl: string;
  docsUrl: string;
  models: FreeModelEntry[];
}

interface RoutePath {
  id: string;
  d: string;
}

interface RouteArrow {
  x: number;
  y: number;
}

function routeBranchToBus(x: number, top: number, busY: number, hubX: number): string {
  const dx = hubX - x;
  if (Math.abs(dx) < 1) return `V ${busY}`;
  const radius = Math.min(12, (top - busY) / 2, Math.abs(dx) / 2);
  return `V ${busY + radius} A ${radius} ${radius} 0 0 ${dx > 0 ? 1 : 0} ${x + Math.sign(dx) * radius} ${busY} H ${hubX}`;
}

const FreeModelsContext = createContext<FreeModelsContextValue | null>(null);
const emptyCatalog: FreeModelDirectory = {
  version: 1,
  updatedAt: '',
  models: [],
};
const providerPriority = ['nvidia-nim', 'openrouter'];

function isKeylessFreeModel(model: FreeModelEntry): boolean {
  return /^Leave the API key empty\./.test(model.notes);
}

function providerRank(model: FreeModelEntry): number {
  const rank = providerPriority.indexOf(model.providerId);
  return rank === -1 ? providerPriority.length : rank;
}

function groupModelsForScan(models: FreeModelEntry[]): FreeModelEntry[][] {
  const groups = new Map<string, FreeModelEntry[]>();
  models.forEach((model) => {
    const id = `${model.providerId}:${model.baseUrl}`;
    const group = groups.get(id);
    if (group) group.push(model);
    else groups.set(id, [model]);
  });
  return [...groups.values()].sort((left, right) => providerRank(left[0]) - providerRank(right[0]));
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

export function useFreeModels() {
  const value = useContext(FreeModelsContext);
  if (!value) throw new Error('useFreeModels must be used within FreeModelsProvider');
  return value;
}

export function FreeModelsProvider({ children }: { children: ReactNode }) {
  const { t } = useI18n();
  const { showToast } = useToast();
  const activePage = useNavigationStore((state) => state.activePage);
  const [viewMode, setViewMode] = useState<'smart' | 'super'>('smart');
  const [catalog, setCatalog] = useState<FreeModelDirectory>(emptyCatalog);
  const [customModels, setCustomModels] = useState<RouteModelNode[]>([]);
  const [proxyAccounts, setProxyAccounts] = useState<
    Partial<Record<AccountProvider, ProxyAccount[]>>
  >({});
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [refreshing, setRefreshing] = useState(false);
  const [scanProvider, setScanProvider] = useState('');
  const [scanProgress, setScanProgress] = useState(0);
  const [routerBaseUrl, setRouterBaseUrl] = useState('127.0.0.1:53683/v1');
  const [routerEnabled, setRouterEnabledState] = useState(true);
  const [routerTogglePending, setRouterTogglePending] = useState(false);
  const [routerLoaded, setRouterLoaded] = useState(false);
  const [savingModelIds, setSavingModelIds] = useState<Set<string>>(() => new Set());
  const savingModelIdsRef = useRef(new Set<string>());
  const refreshInFlightRef = useRef(false);
  const catalogRefreshedRef = useRef(false);
  const routerMutationRef = useRef<Promise<void>>(Promise.resolve());
  const selectedIdsRef = useRef<Set<string>>(new Set());
  const routerLoadedRef = useRef(false);
  const models = catalog.models;
  const accountProvidersKey = JSON.stringify(
    [
      ...new Set(
        customModels.flatMap((model) => (model.accountSource ? [model.accountSource.provider] : []))
      ),
    ].sort()
  );

  useEffect(() => {
    if (activePage !== 'freeModels' || viewMode !== 'smart') return;
    const providers: AccountProvider[] = JSON.parse(accountProvidersKey);
    if (providers.length === 0) return;
    let ignore = false;
    void Promise.allSettled(providers.map((provider) => listProxyAccounts(provider))).then(
      (results) => {
        if (ignore) return;
        setProxyAccounts((previous) => {
          const next = { ...previous };
          results.forEach((result, index) => {
            if (result.status === 'fulfilled') next[providers[index]] = result.value;
          });
          return next;
        });
      }
    );
    return () => {
      ignore = true;
    };
  }, [activePage, viewMode, accountProvidersKey]);

  useEffect(() => {
    let cancelled = false;
    void api
      .getCachedFreeModelDirectory()
      .then((cached) => {
        if (!cancelled && !catalogRefreshedRef.current && cached) setCatalog(cached);
      })
      .catch((error) => console.error('Load cached free model directory failed:', error));
    return () => {
      cancelled = true;
    };
  }, []);

  const loadRouter = useCallback(async () => {
    routerLoadedRef.current = false;
    const [loadedRouter, configuredModels] = await Promise.all([
      api.getSmartRouterConfig(),
      api.getSmartRouterCandidates(),
    ]);
    const modelsById = new Map(configuredModels.map((model) => [model.internalId, model]));
    const routeModels = loadedRouter.candidateIds.flatMap((internalId) => {
      const model = modelsById.get(internalId);
      if (!model) return [];
      return [
        {
          id: internalId,
          internalId,
          provider: model.name,
          modelId: model.modelId ?? '',
          baseUrl: model.baseUrl,
          anthropicUrl: model.anthropicUrl,
          apiKey: model.apiKey,
          accountSource: model.accountSource,
          scope: model.scope,
        },
      ];
    });
    const router =
      loadedRouter.enabled && routeModels.length === 0
        ? await api.setSmartRouterEnabled(false)
        : loadedRouter;
    setCustomModels(routeModels);
    const next = new Set(router.candidateIds);
    selectedIdsRef.current = next;
    setSelectedIds((current) => {
      if (
        current.size === next.size &&
        [...current].every((id, index) => id === router.candidateIds[index])
      ) {
        return current;
      }
      return next;
    });
    setRouterBaseUrl(router.baseUrl.replace(/^https?:\/\//, ''));
    setRouterEnabledState(router.enabled);
    setRouterLoaded(true);
    routerLoadedRef.current = true;
  }, []);

  useEffect(() => {
    if (routerLoadedRef.current && activePage !== 'freeModels') return;
    // Keep reads and their state updates in the mutation queue, so a late
    // page-entry response cannot overwrite a newer addition or priority order.
    const load = routerMutationRef.current.catch(() => undefined).then(loadRouter);
    routerMutationRef.current = load;
    void load.catch((error) => console.error('Load smart router config failed:', error));
  }, [activePage, loadRouter]);

  const setRouterEnabled = useCallback(
    async (enabled: boolean) => {
      setRouterTogglePending(true);
      const change = routerMutationRef.current
        .catch(() => undefined)
        .then(async () => {
          const router = await api.setSmartRouterEnabled(enabled);
          setRouterEnabledState(router.enabled);
          setRouterBaseUrl(router.baseUrl.replace(/^https?:\/\//, ''));
        });
      routerMutationRef.current = change;
      try {
        await change;
      } catch (error) {
        console.error('Toggle smart router failed:', error);
        showToast('error', t('error.requestFailed'));
      } finally {
        setRouterTogglePending(false);
      }
    },
    [showToast, t]
  );

  const refresh = useCallback(async () => {
    if (refreshInFlightRef.current) return;
    refreshInFlightRef.current = true;
    setScanProvider('');
    setScanProgress(0);
    setRefreshing(true);
    try {
      const remote = await api.getFreeModelDirectory();
      if (!remote) {
        showToast('warning', t('freeModels.fetchFailed'));
        return;
      }

      catalogRefreshedRef.current = true;
      setCatalog({ ...remote, models: [] });
      const groups = groupModelsForScan(remote.models);
      const revealedModels: FreeModelEntry[] = [];
      for (const [index, group] of groups.entries()) {
        setScanProvider(group[0]?.provider ?? '');
        await wait(SCAN_STEP_MS);
        revealedModels.push(...group);
        setCatalog({ ...remote, models: [...revealedModels] });
        setScanProgress((index + 1) / groups.length);
      }
      await wait(SCAN_COMPLETE_MS);
    } catch (error) {
      console.error('Fetch free model directory failed:', error);
      showToast('error', t('freeModels.fetchFailed'));
    } finally {
      refreshInFlightRef.current = false;
      setScanProvider('');
      setScanProgress(0);
      setRefreshing(false);
    }
  }, [showToast, t]);

  const addSelectedModel = useCallback(
    async (model: RouteModelInput) => {
      const id = model.internalId;
      const addition = routerMutationRef.current
        .catch(() => undefined)
        .then(async () => {
          // Retry a failed initial/refresh read before constructing a full-list
          // write. An unread configuration must never be treated as an empty one.
          if (!routerLoadedRef.current) await loadRouter();
          const candidateIds = [...new Set([...selectedIdsRef.current, id])];
          const router = await api.setSmartRouterCandidates(candidateIds);
          if (!router.candidateIds.includes(id)) {
            throw new Error(`Smart Router rejected candidate: ${id}`);
          }

          const nextIds = new Set(router.candidateIds);
          selectedIdsRef.current = nextIds;
          setSelectedIds(nextIds);
          setCustomModels((current) => [
            ...current.filter((entry) => entry.id !== id),
            {
              id,
              internalId: model.internalId,
              provider: model.name,
              modelId: model.modelId,
              baseUrl: model.baseUrl,
              anthropicUrl: model.anthropicUrl,
              apiKey: model.apiKey,
              accountSource: model.accountSource,
              scope: model.scope,
            },
          ]);
          setRouterBaseUrl(router.baseUrl.replace(/^https?:\/\//, ''));
        });
      routerMutationRef.current = addition;
      await addition;
    },
    [loadRouter]
  );

  const updateSelectedModel = useCallback(
    (model: RouteModelInput) => {
      routerMutationRef.current = routerMutationRef.current
        .catch(() => undefined)
        .then(async () => {
          setCustomModels((current) =>
            current.map((entry) =>
              entry.id === model.internalId
                ? {
                    ...entry,
                    provider: model.name,
                    modelId: model.modelId,
                    baseUrl: model.baseUrl,
                    anthropicUrl: model.anthropicUrl,
                    apiKey: model.apiKey,
                    accountSource: model.accountSource,
                    scope: model.scope ?? entry.scope,
                  }
                : entry
            )
          );
          await loadRouter();
        })
        .catch((error) => console.error('Refresh smart router after model update failed:', error));
    },
    [loadRouter]
  );

  const changeSelectedModel = useCallback(
    async (id: string, modelId: string) => {
      if (savingModelIdsRef.current.has(id)) return;
      savingModelIdsRef.current.add(id);
      setSavingModelIds(new Set(savingModelIdsRef.current));
      const change = routerMutationRef.current
        .catch(() => undefined)
        .then(async () => {
          if (!routerLoadedRef.current) await loadRouter();
          if (!selectedIdsRef.current.has(id))
            throw new Error(`Smart router model not found: ${id}`);
          const updated = await api.updateModel(id, { modelId });
          if (!updated) throw new Error(`Smart router model not found: ${id}`);
          setCustomModels((current) =>
            current.map((entry) =>
              entry.id === id ? { ...entry, modelId: updated.modelId ?? modelId } : entry
            )
          );
          return updated;
        });
      routerMutationRef.current = change.then(
        () => undefined,
        () => undefined
      );
      try {
        return await change;
      } finally {
        savingModelIdsRef.current.delete(id);
        setSavingModelIds(new Set(savingModelIdsRef.current));
      }
    },
    [loadRouter]
  );

  const removeSelectedModel = useCallback(
    async (id: string) => {
      const removal = routerMutationRef.current
        .catch(() => undefined)
        .then(async () => {
          if (!routerLoadedRef.current) await loadRouter();
          let router = await api.removeSmartRouterCandidate(id);
          if (router.enabled && router.candidateIds.length === 0) {
            router = await api.setSmartRouterEnabled(false);
          }
          const nextIds = new Set(router.candidateIds);
          selectedIdsRef.current = nextIds;
          setSelectedIds(nextIds);
          setCustomModels((current) => current.filter((model) => model.id !== id));
          setRouterBaseUrl(router.baseUrl.replace(/^https?:\/\//, ''));
          setRouterEnabledState(router.enabled);
        });
      routerMutationRef.current = removal;
      try {
        await removal;
      } catch (error) {
        console.error('Remove smart router candidate failed:', error);
        showToast('error', t('error.requestFailed'));
      }
    },
    [loadRouter, showToast, t]
  );

  const reorderSelectedModel = useCallback(
    async (activeId: string, overId: string) => {
      const reorder = routerMutationRef.current
        .catch(() => undefined)
        .then(async () => {
          if (!routerLoadedRef.current) await loadRouter();
          const previous = selectedIdsRef.current;
          const ids = [...previous];
          const from = ids.indexOf(activeId);
          const to = ids.indexOf(overId);
          if (from < 0 || to < 0 || from === to) return;
          const candidateIds = arrayMove(ids, from, to);
          setSelectedIds(new Set(candidateIds));
          try {
            const router = await api.setSmartRouterCandidates(candidateIds);
            const next = new Set(router.candidateIds);
            selectedIdsRef.current = next;
            setSelectedIds(next);
          } catch (error) {
            setSelectedIds(previous);
            throw error;
          }
        });
      routerMutationRef.current = reorder;
      try {
        await reorder;
      } catch (error) {
        console.error('Save smart router order failed:', error);
        showToast('error', t('error.requestFailed'));
      }
    },
    [loadRouter, showToast, t]
  );

  return (
    <FreeModelsContext.Provider
      value={{
        viewMode,
        setViewMode,
        catalog,
        models,
        customModels,
        proxyAccounts,
        selectedIds,
        refreshing,
        scanProvider,
        scanProgress,
        routerBaseUrl,
        routerEnabled,
        routerTogglePending: routerTogglePending || !routerLoaded,
        setRouterEnabled,
        refresh,
        addSelectedModel,
        updateSelectedModel,
        changeSelectedModel,
        savingModelIds,
        removeSelectedModel,
        reorderSelectedModel,
      }}
    >
      {children}
    </FreeModelsContext.Provider>
  );
}

export function FreeModelsTitleActions() {
  const { t } = useI18n();
  const { viewMode, setViewMode } = useFreeModels();
  const [showHelp, setShowHelp] = useState(false);

  useEffect(() => {
    if (!showHelp) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setShowHelp(false);
    };
    window.addEventListener('keydown', closeOnEscape);
    return () => window.removeEventListener('keydown', closeOnEscape);
  }, [showHelp]);

  return (
    <div className="flex shrink-0 items-center gap-3">
      <ViewModeTabs
        value={viewMode}
        onChange={setViewMode}
        options={[
          { value: 'smart', label: t('freeModels.view.smart') },
          { value: 'super', label: t('freeModels.view.super') },
        ]}
      />
      <button
        type="button"
        onClick={() => setShowHelp(true)}
        className="flex items-center gap-1.5 text-sm font-mono px-3 py-1.5 border border-cyber-border rounded-button text-cyber-text hover:bg-cyber-text/10 transition-colors"
      >
        <CircleHelp size={13} />
        {t('freeModels.help')}
      </button>

      {showHelp && (
        <div className="fixed inset-0 z-[9998] flex items-center justify-center">
          <div className="absolute inset-0 bg-black/60" onClick={() => setShowHelp(false)} />
          <div className="relative w-[520px] max-w-[90vw] border border-cyber-border/30 bg-cyber-surface shadow-2xl rounded-xl overflow-hidden">
            <div className="h-px w-full bg-cyber-border" />
            <button
              type="button"
              onClick={() => setShowHelp(false)}
              aria-label={t('btn.close')}
              className="absolute right-5 top-5 text-cyber-text-secondary hover:text-cyber-text transition-colors"
            >
              <X size={18} />
            </button>

            <div className="px-6 pt-6 pb-6 pr-14 space-y-4">
              <div className="flex gap-3">
                <Waypoints size={18} className="mt-0.5 flex-shrink-0 text-cyber-accent" />
                <div>
                  <div className="text-sm font-semibold text-cyber-text">
                    {t('freeModels.help.routerTitle')}
                  </div>
                  <p className="mt-1 text-xs leading-5 text-cyber-text-secondary">
                    {t('freeModels.help.routerDesc')}
                  </p>
                </div>
              </div>

              <div className="h-px bg-cyber-border/60" />

              <div className="flex gap-3">
                <KeyRound size={18} className="mt-0.5 flex-shrink-0 text-cyber-accent" />
                <div>
                  <div className="text-sm font-semibold text-cyber-text">
                    {t('freeModels.help.useTitle')}
                  </div>
                  <ol className="mt-1 space-y-1.5 text-xs leading-5 text-cyber-text-secondary list-decimal list-inside">
                    <li>{t('freeModels.help.step1')}</li>
                    <li>{t('freeModels.help.step2')}</li>
                    <li>{t('freeModels.help.step3')}</li>
                  </ol>
                </div>
              </div>
            </div>

            <button
              type="button"
              onClick={() => setShowHelp(false)}
              className="w-full border-t border-cyber-border px-4 py-3 text-[14px] font-semibold text-cyber-text hover:bg-cyber-text/10 transition-colors"
            >
              {t('btn.close')}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function RouterHub({
  title,
  enabled,
  baseUrl,
  hubRef,
  onCopy,
}: {
  title: string;
  enabled: boolean;
  baseUrl: string;
  hubRef?: ComponentPropsWithRef<'div'>['ref'];
  onCopy?: (endpoint: string) => void;
}) {
  const { t } = useI18n();
  return (
    <div
      ref={hubRef}
      className="free-model-router-hub relative z-10 mx-auto w-full max-w-[380px] rounded-xl p-5"
    >
      <div className="grid h-8 grid-cols-[22px_minmax(0,1fr)_auto] items-center gap-2">
        <Waypoints size={22} aria-hidden="true" className="text-cyber-accent" />
        <div className="min-w-0 truncate text-lg font-semibold leading-none text-cyber-text">
          {title}
        </div>
        <span
          className={`flex items-center gap-1.5 whitespace-nowrap text-xs font-medium leading-none ${enabled ? 'text-green-500 [[data-theme=light]_&]:text-green-800' : 'text-cyber-text-muted'}`}
        >
          <span className="h-1.5 w-1.5 rounded-full bg-current" aria-hidden="true" />
          {t(enabled ? 'freeModels.router.enabled' : 'freeModels.router.disabled')}
        </span>
      </div>
      <div className="mt-4 grid grid-cols-2 gap-3">
        {[
          ['OpenAI', baseUrl],
          ['Anthropic', baseUrl.replace(/\/v1\/?$/, '')],
        ].map(([protocol, endpoint]) => {
          const content = (
            <>
              <span className="flex h-5 items-center text-[10px] font-semibold text-cyber-text-muted">
                {protocol}
              </span>
              <span className="mt-1 block break-all text-xs font-mono text-cyber-text-secondary">
                {endpoint}
              </span>
            </>
          );
          const className =
            'block min-w-0 rounded-button bg-cyber-bg/60 px-3 py-2 text-left select-none';
          return onCopy ? (
            <button
              key={protocol}
              type="button"
              aria-label={`${t('btn.copy')} ${protocol}`}
              onClick={() => onCopy(endpoint)}
              className={className}
            >
              {content}
            </button>
          ) : (
            <div key={protocol} className={className}>
              {content}
            </div>
          );
        })}
      </div>
    </div>
  );
}

interface RouteModelCardProps {
  model: RouteModelNode;
  account?: ProxyAccount;
  keyless?: boolean;
  providerIconSrc?: string | null;
  priority: number;
  active?: boolean;
  compact?: boolean;
  modelIdOptions?: string[];
  freeModelIds?: string[];
  paidModelIds?: string[];
  optionMultipliers?: Record<string, number>;
  onLoadModels?: () => void;
  refreshing?: boolean;
  saving?: boolean;
  dragging?: boolean;
  onModelChange?: (id: string, modelId: string) => Promise<void>;
}

function RouteModelCard({
  model,
  account,
  keyless = false,
  providerIconSrc,
  priority,
  active = false,
  compact = false,
  modelIdOptions = [],
  freeModelIds,
  paidModelIds,
  optionMultipliers,
  onLoadModels,
  refreshing = false,
  saving = false,
  dragging = false,
  onModelChange,
  dragHandleProps,
  overlay = false,
}: RouteModelCardProps & {
  dragHandleProps?: ComponentPropsWithRef<'button'>;
  overlay?: boolean;
}) {
  const { t } = useI18n();
  const [pickerOpen, setPickerOpen] = useState(false);
  const cardRef = useRef<HTMLDivElement>(null);
  const dragPointerDown: PointerEventHandler<HTMLElement> | undefined =
    dragHandleProps?.onPointerDown;
  const accountProvider = accountProxyProviders.find((p) => p.id === model.accountSource?.provider);
  const displayName = model.accountSource
    ? (account && accountProxyIdentityLabel(account, t)) || accountProvider?.name || model.provider
    : model.provider;
  const displayPlan = account ? accountProxyPlanLabel(account, t).trim() : '';
  const iconSrc = accountProvider
    ? `/icons/tools/${accountProvider.icon}`
    : providerIconSrc !== undefined
      ? providerIconSrc
      : getModelIcon(model.provider, model.modelId);
  const hasModelPicker = Boolean(onModelChange && modelIdOptions.length > 0 && !overlay);
  if (pickerOpen && (!hasModelPicker || saving || dragging)) setPickerOpen(false);
  const changePickerOpen = (open: boolean) => {
    if (open && (!hasModelPicker || saving || dragging)) return;
    setPickerOpen(open);
    if (open && !pickerOpen) onLoadModels?.();
  };

  return (
    <div
      ref={cardRef}
      aria-hidden={overlay || undefined}
      className={`free-model-route-node relative min-h-[80px] rounded-card p-3 flex items-center ${hasModelPicker ? 'has-model-picker' : ''} ${active ? 'is-active' : ''} ${overlay ? 'is-dragging shadow-2xl' : ''}`}
    >
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 overflow-hidden rounded-[inherit]"
      >
        <span
          className={`free-model-route-priority absolute ${compact ? 'is-compact' : 'right-8'} font-mono font-semibold`}
        >
          {priority}
        </span>
      </div>
      {(dragHandleProps || overlay) && (
        <button
          {...dragHandleProps}
          type="button"
          tabIndex={overlay ? -1 : dragHandleProps?.tabIndex}
          aria-label={`${t('freeModels.router.priority')} ${priority}: ${model.modelId}`}
          aria-expanded={hasModelPicker ? pickerOpen : undefined}
          onClick={() => {
            if (hasModelPicker && !saving && !dragging) changePickerOpen(!pickerOpen);
          }}
          onKeyDown={(event) => {
            if (hasModelPicker && !saving && !dragging && event.key === 'Enter') {
              event.preventDefault();
              changePickerOpen(true);
            } else {
              dragHandleProps?.onKeyDown?.(event);
            }
          }}
          className="absolute inset-0 rounded-card touch-none focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-cyber-accent"
        />
      )}
      <div className="relative z-10 pointer-events-none flex w-full items-center gap-2.5">
        {iconSrc ? (
          <img src={iconSrc} alt="" className="h-12 w-12 shrink-0 object-contain" />
        ) : (
          <Box size={48} className="shrink-0 text-cyber-text-secondary" aria-hidden="true" />
        )}
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <div
            className={
              compact ? 'flex h-5 min-w-0 items-center gap-2' : 'flex min-w-0 items-center gap-1'
            }
          >
            <span
              className={`min-w-0 flex-1 text-sm ${compact ? '!leading-5' : ''} font-semibold text-cyber-text truncate`}
            >
              {compact ? displayName : model.provider}
            </span>
            {compact && keyless && <KeylessModelLabel card />}
            {compact && model.accountSource && displayPlan && (
              <span className="shrink-0 whitespace-nowrap text-xs text-cyber-text-secondary">
                {displayPlan}
              </span>
            )}
          </div>
          {hasModelPicker ? (
            <div
              className="free-model-route-picker pointer-events-auto min-w-0"
              aria-busy={saving || refreshing}
              onPointerDown={(event) => {
                event.stopPropagation();
                if (event.currentTarget.contains(event.target as Node)) dragPointerDown?.(event);
              }}
              onKeyDown={(event) => event.stopPropagation()}
            >
              <ModelIdCombobox
                compact
                selectOnly
                value={model.modelId}
                options={modelIdOptions}
                optionLabels={account?.modelLabels}
                freeModelIds={account?.freeModelIds ?? freeModelIds}
                paidModelIds={paidModelIds}
                optionMultipliers={optionMultipliers}
                freeLabel={t('model.free')}
                refreshing={refreshing}
                disabled={saving || dragging}
                open={pickerOpen}
                onOpenChange={changePickerOpen}
                externalTriggerRef={cardRef}
                ariaLabel={`${t('model.modelId')}: ${displayName} (${priority})`}
                onChange={(modelId) => void onModelChange?.(model.id, modelId)}
              />
            </div>
          ) : (
            <span className="block min-w-0 text-xs text-cyber-text-secondary truncate">
              {account?.modelLabels?.[model.modelId] ?? model.modelId}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function SortableRouteModel({
  registerNode,
  ...props
}: RouteModelCardProps & {
  registerNode: (id: string, node: HTMLDivElement | null) => void;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    setActivatorNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: props.model.id });

  return (
    <div ref={(node) => registerNode(props.model.id, node)} className="min-w-0">
      <div
        ref={setNodeRef}
        style={{
          transform: CSS.Transform.toString(transform),
          transition,
          opacity: isDragging ? 0 : undefined,
        }}
      >
        <RouteModelCard
          {...props}
          compact
          dragging={isDragging}
          dragHandleProps={{ ref: setActivatorNodeRef, ...attributes, ...listeners }}
        />
      </div>
    </div>
  );
}

function RouterRemoveDropZone({
  visible,
  stageRef,
}: {
  visible: boolean;
  stageRef: RefObject<HTMLDivElement>;
}) {
  const { t } = useI18n();
  const { setNodeRef, isOver } = useDroppable({
    id: ROUTER_REMOVE_DROP_ID,
    disabled: !visible,
  });
  const { measureDroppableContainers } = useDndContext();
  const [position, setPosition] = useState({ left: 0, width: 0, bottom: 12 });

  useLayoutEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const update = () => {
      const rect = stage.getBoundingClientRect();
      const next = {
        left: rect.left + NODE_SIDE_PADDING,
        width: Math.max(0, rect.width - NODE_SIDE_PADDING * 2),
        bottom: Math.max(12, window.innerHeight - rect.bottom + 12),
      };
      setPosition((current) =>
        current.left === next.left && current.width === next.width && current.bottom === next.bottom
          ? current
          : next
      );
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(stage);
    window.addEventListener('resize', update);
    window.addEventListener('scroll', update, true);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', update);
      window.removeEventListener('scroll', update, true);
    };
  }, [stageRef, visible]);

  useLayoutEffect(() => {
    if (visible && position.width > 0) measureDroppableContainers([ROUTER_REMOVE_DROP_ID]);
  }, [visible, position, measureDroppableContainers]);

  return (
    <div
      ref={setNodeRef}
      id={ROUTER_REMOVE_DROP_ID}
      aria-hidden={!visible}
      className="pointer-events-none fixed z-[1001] h-16"
      style={position}
    >
      <div
        className={`free-model-remove-zone flex h-full items-center justify-center gap-3 rounded-xl px-4 font-sans text-sm ${visible ? 'is-visible' : ''} ${visible && isOver ? 'is-over' : ''}`}
        role="status"
        aria-live="polite"
      >
        <Trash2 size={24} aria-hidden="true" />
        <span>
          {t(isOver ? 'freeModels.router.releaseToRemove' : 'freeModels.router.dropToRemove')}
        </span>
      </div>
    </div>
  );
}

const SUPER_ROUTER_MODELS: RouteModelNode[] = [
  ['Anthropic', 'claude-opus-5-5'],
  ['OpenAI', 'gpt-6-astra'],
  ['Google Gemini', 'gemini-3.7-flash'],
  ['DeepSeek', 'deepseek-v4-pro'],
  ['Z.ai', 'glm-5.3'],
  ['MiniMax', 'MiniMax-M3'],
  ['Qwen', 'qwen3.8-max'],
].map(([provider, modelId], index) => ({
  id: `super-preview-${index}`,
  internalId: `super-preview-${index}`,
  provider,
  modelId,
  baseUrl: '',
}));

function SuperRouterMain() {
  const { t } = useI18n();
  const stageRef = useRef<HTMLDivElement | null>(null);
  const hubRef = useRef<HTMLDivElement | null>(null);
  const leadRef = useRef<HTMLDivElement | null>(null);
  const gridRef = useRef<HTMLDivElement | null>(null);
  const [diagram, setDiagram] = useState({ width: 1, height: 1, paths: '', arrows: '' });

  useLayoutEffect(() => {
    const stage = stageRef.current;
    const hub = hubRef.current;
    const lead = leadRef.current;
    const grid = gridRef.current;
    if (!stage || !hub || !lead || !grid) return;
    const update = () => {
      const bounds = stage.getBoundingClientRect();
      const box = (node: Element) => {
        const rect = node.getBoundingClientRect();
        return {
          left: rect.left - bounds.left,
          right: rect.right - bounds.left,
          top: rect.top - bounds.top,
          bottom: rect.bottom - bounds.top,
          x: rect.left - bounds.left + rect.width / 2,
          y: rect.top - bounds.top + rect.height / 2,
        };
      };
      const hubBox = box(hub);
      const leadBox = box(lead);
      const nodes = [...grid.children].map(box);
      const firstRow = nodes.filter((node) => Math.abs(node.top - nodes[0].top) < 12);
      const busY = (Math.max(hubBox.bottom, leadBox.bottom) + firstRow[0].top) / 2;
      const stacked = leadBox.top >= hubBox.bottom;
      const paths = [
        stacked
          ? `M ${leadBox.x} ${leadBox.top - 3} V ${hubBox.bottom + 18}`
          : `M ${leadBox.left - 3} ${hubBox.y} H ${hubBox.right + 18}`,
        `M ${leadBox.x} ${busY} V ${leadBox.bottom + 18}`,
      ];
      nodes.forEach((node, index) => {
        const parent = nodes
          .slice(0, index)
          .reverse()
          .find((entry) => Math.abs(entry.x - node.x) < 1);
        const outerBranch =
          !parent &&
          firstRow.length > 1 &&
          (node === firstRow[0] || node === firstRow[firstRow.length - 1]);
        paths.push(
          `M ${node.x} ${node.top - 3} ${outerBranch ? routeBranchToBus(node.x, node.top - 3, busY, leadBox.x) : `V ${parent ? parent.bottom + 3 : busY}`}`
        );
      });
      setDiagram({
        width: bounds.width,
        height: bounds.height,
        paths: paths.join(' '),
        arrows: `${
          stacked
            ? `M ${hubBox.x} ${hubBox.bottom + 4} L ${hubBox.x - 6} ${hubBox.bottom + 16} H ${hubBox.x + 6} Z`
            : `M ${hubBox.right + 4} ${hubBox.y} L ${hubBox.right + 16} ${hubBox.y - 6} V ${hubBox.y + 6} Z`
        } M ${leadBox.x} ${leadBox.bottom + 4} L ${leadBox.x - 6} ${leadBox.bottom + 16} H ${leadBox.x + 6} Z`,
      });
    };
    const frame = requestAnimationFrame(update);
    const observer = new ResizeObserver(update);
    [stage, hub, lead, grid].forEach((element) => observer.observe(element));
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, []);

  return (
    <div className="free-model-router super-router cursor-default">
      <div ref={stageRef} className="is-disabled relative">
        <svg
          className="pointer-events-none absolute inset-0 h-full w-full"
          viewBox={`0 0 ${diagram.width} ${diagram.height}`}
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          <path d={diagram.paths} className="free-model-route-path" />
          <path d={diagram.arrows} className="free-model-route-arrow" />
        </svg>
        <div className="super-router-header">
          <RouterHub
            hubRef={hubRef}
            title={t('freeModels.super.title')}
            enabled={false}
            baseUrl="127.0.0.1:62186/v1"
          />
          <div ref={leadRef} className="relative z-10 min-w-0">
            <RouteModelCard model={SUPER_ROUTER_MODELS[0]} priority={0} />
          </div>
        </div>
        <div ref={gridRef} className="super-router-grid relative z-10">
          {SUPER_ROUTER_MODELS.slice(1).map((model, index) => (
            <RouteModelCard key={model.id} model={model} priority={index + 1} />
          ))}
        </div>
      </div>
      <p className="mx-auto mt-14 max-w-[680px] cursor-default text-sm leading-6 text-cyber-text-secondary">
        {t('freeModels.super.description')}{' '}
        <span className="font-medium text-red-500">{t('freeModels.super.unavailable')}</span>
      </p>
    </div>
  );
}

export function FreeModelsMain() {
  const { viewMode } = useFreeModels();
  return viewMode === 'super' ? <SuperRouterMain /> : <SmartRouterMain />;
}

type RouteCatalogInput = modelApi.UpstreamModelCatalogInput | { accountSource: AccountSource };
type RouteModelCatalog = modelApi.UpstreamModelCatalog & {
  account?: ProxyAccount;
  accountCacheSnapshot?: string;
};

function routeCatalogBinding(model: RouteModelNode) {
  if (model.id === 'local-server') return null;
  if (model.accountSource) {
    const { provider, accountId } = model.accountSource;
    const input: RouteCatalogInput = { accountSource: { provider, accountId } };
    return { input, key: JSON.stringify(input) };
  }
  if (model.apiKey === undefined) return null;
  const input: RouteCatalogInput = {
    baseUrl: model.baseUrl.trim(),
    anthropicUrl: model.anthropicUrl?.trim() ?? '',
    apiKey: model.apiKey.trim(),
  };
  return { input, key: JSON.stringify(input) };
}

function SmartRouterMain() {
  const { t } = useI18n();
  const { showToast } = useToast();
  const { updateUserModel } = useModelNexus();
  const {
    models: freeModels,
    customModels,
    proxyAccounts,
    selectedIds,
    routerBaseUrl,
    routerEnabled,
    removeSelectedModel,
    reorderSelectedModel,
    changeSelectedModel,
    savingModelIds,
  } = useFreeModels();
  const [routeCatalogs, setRouteCatalogs] = useState<Map<string, RouteModelCatalog>>(
    () => new Map()
  );
  const [refreshingCatalogs, setRefreshingCatalogs] = useState<Set<string>>(() => new Set());
  const catalogGeneration = useRef(0);
  const catalogRequests = useRef(new Map<string, object>());
  const catalogRevisions = useRef(new Map<string, number>());
  const loadedCatalogs = useRef(new Set<string>());
  const catalogBindingsKey = JSON.stringify(
    [
      ...new Set(
        customModels.flatMap((model) => {
          const binding = routeCatalogBinding(model);
          return binding ? [binding.key] : [];
        })
      ),
    ].sort()
  );
  const currentCatalogBindings = useRef(new Set<string>());

  useEffect(() => {
    const generation = ++catalogGeneration.current;
    const keys: string[] = JSON.parse(catalogBindingsKey);
    currentCatalogBindings.current = new Set(keys);
    const requests = catalogRequests.current;
    keys.forEach((key) => {
      const revision = catalogRevisions.current.get(key);
      const input: RouteCatalogInput = JSON.parse(key);
      const models =
        'accountSource' in input ? getCachedAccountProxyModels(input.accountSource) : undefined;
      const cached =
        'accountSource' in input
          ? Promise.resolve(
              models === undefined
                ? null
                : {
                    models: models.map((id) => ({ id })),
                    requiresApiKey: false,
                    accountCacheSnapshot: JSON.stringify(models),
                  }
            )
          : modelApi.getCachedUpstreamModelCatalog(input);
      void cached
        .then((catalog) => {
          if (
            catalog &&
            catalogGeneration.current === generation &&
            catalogRevisions.current.get(key) === revision
          ) {
            setRouteCatalogs((current) =>
              'accountSource' in input && current.has(key)
                ? current
                : new Map(current).set(key, catalog)
            );
          }
        })
        .catch(() => undefined);
    });
    return () => {
      catalogGeneration.current = generation + 1;
      requests.clear();
      setRefreshingCatalogs(new Set());
    };
  }, [catalogBindingsKey]);

  const refreshRouteCatalog = async (model: RouteModelNode) => {
    const binding = routeCatalogBinding(model);
    if (
      !binding ||
      !currentCatalogBindings.current.has(binding.key) ||
      catalogRequests.current.has(binding.key) ||
      loadedCatalogs.current.has(binding.key)
    )
      return;
    const { key, input } = binding;
    const generation = catalogGeneration.current;
    const request = {};
    catalogRequests.current.set(key, request);
    catalogRevisions.current.set(key, (catalogRevisions.current.get(key) ?? 0) + 1);
    setRefreshingCatalogs((current) => new Set(current).add(key));
    const isCurrent = () =>
      catalogGeneration.current === generation && currentCatalogBindings.current.has(key);
    try {
      let catalog: RouteModelCatalog;
      if ('accountSource' in input) {
        const source = input.accountSource;
        const models = await getAccountProxyModels(source);
        if (!isCurrent()) return;
        let account: ProxyAccount | undefined;
        try {
          const accounts = await listProxyAccounts(source.provider);
          account = accounts.find(
            (entry) =>
              entry.source.provider === source.provider &&
              entry.source.accountId === source.accountId
          );
        } catch (error) {
          console.error('Load proxy model labels failed:', error);
        }
        if (!isCurrent()) return;
        cacheAccountProxyModels(source, models);
        catalog = {
          models: models.map((id) => ({ id })),
          requiresApiKey: false,
          account,
          accountCacheSnapshot: JSON.stringify(getCachedAccountProxyModels(source) ?? null),
        };
      } else {
        catalog = await modelApi.fetchUpstreamModelCatalog(input);
      }
      if (isCurrent()) {
        loadedCatalogs.current.add(key);
        setRouteCatalogs((current) => new Map(current).set(key, catalog));
      }
    } catch {
      // Keep cached choices on an automatic load failure; reopening can retry.
    } finally {
      if (catalogRequests.current.get(key) === request) {
        catalogRequests.current.delete(key);
        if (isCurrent())
          setRefreshingCatalogs((current) => {
            const next = new Set(current);
            next.delete(key);
            return next;
          });
      }
    }
  };
  const routeCatalogFor = (model: RouteModelNode) => {
    const binding = routeCatalogBinding(model);
    return binding ? routeCatalogs.get(binding.key) : undefined;
  };
  const modelMetadataFor = (model: RouteModelNode) => {
    const binding = routeCatalogBinding(model);
    const refresh = {
      refreshing: binding ? refreshingCatalogs.has(binding.key) : false,
      onLoadModels: binding ? () => void refreshRouteCatalog(model) : undefined,
    };
    if (model.accountSource || model.id === 'local-server') return refresh;
    const upstream = routeCatalogFor(model)?.models ?? [];
    const paidModelIds = upstream
      .filter(
        (entry) => entry.free !== true && (entry.free === false || (entry.multiplier ?? 0) > 0)
      )
      .map((entry) => entry.id);
    const freeModelIds = [
      ...new Set([
        ...freeModels
          .filter(
            (entry) =>
              entry.baseUrl === model.baseUrl &&
              (entry.freeType === 'perpetual' || isKeylessFreeModel(entry)) &&
              !paidModelIds.includes(entry.modelId)
          )
          .map((entry) => entry.modelId),
        ...upstream.filter((entry) => entry.free === true).map((entry) => entry.id),
      ]),
    ];
    const optionMultipliers = Object.fromEntries(
      upstream.flatMap((entry) =>
        entry.multiplier === undefined ? [] : [[entry.id, entry.multiplier]]
      )
    );
    return {
      freeModelIds,
      paidModelIds,
      optionMultipliers,
      ...refresh,
    };
  };
  const accountFor = (model: RouteModelNode) => {
    const source = model.accountSource;
    return source
      ? (routeCatalogFor(model)?.account ??
          proxyAccounts[source.provider]?.find(
            (account) =>
              account.source.provider === source.provider &&
              account.source.accountId === source.accountId
          ))
      : undefined;
  };
  const keylessFor = (model: RouteModelNode) =>
    !model.accountSource &&
    model.id !== 'local-server' &&
    freeModels.some(
      (entry) =>
        entry.baseUrl === model.baseUrl &&
        entry.modelId === model.modelId &&
        isKeylessFreeModel(entry)
    );
  const providerIconFor = (model: RouteModelNode) => {
    if (model.accountSource || model.id === 'local-server' || model.scope !== 'smartRouter')
      return undefined;
    const provider = freeModels.find((entry) => entry.baseUrl === model.baseUrl);
    return provider ? getModelIcon(provider.provider, '') : undefined;
  };
  const modelIdOptionsFor = (model: RouteModelNode) => {
    if (model.id === 'local-server') return [];
    if (model.accountSource) {
      const cached = getCachedAccountProxyModels(model.accountSource);
      const catalog = routeCatalogFor(model);
      const fetchedModels = catalog?.models.map((entry) => entry.id);
      // Keep a successful refresh usable if persistence fails, while observing
      // a later directory update from the account add/edit dialog.
      const models =
        (catalog?.accountCacheSnapshot === JSON.stringify(cached ?? null)
          ? fetchedModels
          : cached) ??
        fetchedModels ??
        [];
      return [
        ...new Set([
          model.modelId,
          ...accountModelPickerOptions(models, model.modelId, accountFor(model)?.modelLabels),
        ]),
      ].filter(Boolean);
    }
    const catalog = routeCatalogFor(model);
    return [
      ...new Set([
        model.modelId,
        ...(catalog
          ? catalog.models.map((entry) => entry.id)
          : freeModels
              .filter((entry) => entry.baseUrl === model.baseUrl)
              .map((entry) => entry.modelId)),
      ]),
    ].filter(Boolean);
  };
  const switchModel = async (id: string, modelId: string) => {
    const model = customModels.find((entry) => entry.id === id);
    if (
      !model ||
      model.modelId === modelId ||
      savingModelIds.has(id) ||
      !modelIdOptionsFor(model).includes(modelId)
    )
      return;
    try {
      const updated = await changeSelectedModel(id, modelId);
      if (!updated) return;
      updateUserModel(updated, {
        name: updated.name,
        baseUrl: updated.baseUrl,
        anthropicUrl: updated.anthropicUrl ?? '',
        apiKey: updated.apiKey,
        modelId: updated.modelId ?? modelId,
        accountSource: updated.accountSource,
      });
    } catch (error) {
      console.error('Change smart router model failed:', error);
      showToast('error', t('error.requestFailed'));
    }
  };
  const selectedModels = useMemo(() => {
    const modelsById = new Map(customModels.map((model) => [model.id, model]));
    return [...selectedIds].flatMap((id) => {
      const model = modelsById.get(id);
      return model ? [model] : [];
    });
  }, [customModels, selectedIds]);
  const [activeDragId, setActiveDragId] = useState<string | null>(null);
  const activeDragModel = selectedModels.find((model) => model.id === activeDragId);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates })
  );
  const handleDragEnd = ({ active, over }: DragEndEvent) => {
    setActiveDragId(null);
    if (over?.id === ROUTER_REMOVE_DROP_ID) {
      if (activeDragId === String(active.id)) void removeSelectedModel(String(active.id));
      return;
    }
    if (over && active.id !== over.id) {
      void reorderSelectedModel(String(active.id), String(over.id));
    }
  };
  const stageRef = useRef<HTMLDivElement | null>(null);
  const hubRef = useRef<HTMLDivElement | null>(null);
  const nodeRefs = useRef(new Map<string, HTMLDivElement>());
  const [routePaths, setRoutePaths] = useState<RoutePath[]>([]);
  const [activityPaths, setActivityPaths] = useState<RoutePath[]>([]);
  const [routeArrow, setRouteArrow] = useState<RouteArrow | null>(null);
  const [canvasSize, setCanvasSize] = useState({ width: 1, height: 1 });
  const [nodeColumns, setNodeColumns] = useState(3);
  const [routerActivity, setRouterActivity] = useState<api.SmartRouterActivity>({
    candidateId: null,
    active: false,
    sequence: 0,
    updatedAtMs: 0,
  });
  const [activityObservedAtMs, setActivityObservedAtMs] = useState(0);

  useEffect(() => {
    let cancelled = false;
    if (!routerEnabled) return;
    let errorReported = false;
    const pollActivity = async () => {
      try {
        const activity = await api.getSmartRouterActivity();
        if (!cancelled) {
          setRouterActivity(activity);
          setActivityObservedAtMs(Date.now());
        }
      } catch (error) {
        if (!cancelled && !errorReported) {
          errorReported = true;
          console.error('Load smart router activity failed:', error);
        }
      }
    };
    void pollActivity();
    const timer = window.setInterval(() => void pollActivity(), ROUTER_ACTIVITY_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [routerEnabled]);

  const setNodeRef = useCallback((id: string, node: HTMLDivElement | null) => {
    if (node) nodeRefs.current.set(id, node);
    else nodeRefs.current.delete(id);
  }, []);

  const updatePaths = useCallback(() => {
    const stage = stageRef.current;
    const hub = hubRef.current;
    if (!stage || !hub) return;
    const stageBox = stage.getBoundingClientRect();
    const hubBox = hub.getBoundingClientRect();
    setNodeColumns(
      Math.max(
        1,
        Math.min(
          3,
          Math.floor(
            (stageBox.width - NODE_SIDE_PADDING * 2 + NODE_COLUMN_GAP) / (160 + NODE_COLUMN_GAP)
          )
        )
      )
    );
    const endX = hubBox.left - stageBox.left + hubBox.width / 2;
    const endY = hubBox.bottom - stageBox.top;

    const entries = [...nodeRefs.current].map(([id, node]) => {
      const box = node.getBoundingClientRect();
      return {
        id,
        x: box.left - stageBox.left + box.width / 2,
        top: box.top - stageBox.top,
        bottom: box.bottom - stageBox.top,
      };
    });
    entries.sort((left, right) => left.top - right.top || left.x - right.x);

    const rows: (typeof entries)[] = [];
    for (const entry of entries) {
      const row = rows.find((candidate) => Math.abs(candidate[0].top - entry.top) < 12);
      if (row) row.push(entry);
      else rows.push([entry]);
    }

    const nextPaths: RoutePath[] = [];
    const nextActivityPaths: RoutePath[] = [];
    const parentById = new Map<string, (typeof entries)[number]>();
    const firstRow = rows[0];
    let nextArrow: RouteArrow | null = null;
    let routeBusY: number | null = null;
    let routeLineEndY: number | null = null;
    if (firstRow) {
      const arrowY = endY + HUB_ARROW_GAP;
      const lineEndY = arrowY + HUB_ARROW_HEIGHT + HUB_ARROW_LINE_GAP;
      routeLineEndY = lineEndY;
      if (firstRow.length === 1) {
        const entry = firstRow[0];
        nextPaths.push({
          id: entry.id,
          d: `M ${entry.x} ${entry.top - 3} V ${lineEndY}`,
        });
        nextArrow = { x: endX, y: arrowY };
      } else {
        const busY = endY + (firstRow[0].top - endY) / 2;
        routeBusY = busY;
        nextPaths.push({
          id: 'hub-trunk',
          d: `M ${endX} ${busY} V ${lineEndY}`,
        });
        nextArrow = { x: endX, y: arrowY };
        const busJoins = firstRow.map((entry) => {
          const top = entry.top - 3;
          const dx = endX - entry.x;
          const radius = Math.min(12, (top - busY) / 2, Math.abs(dx) / 2);
          const joinX = Math.abs(dx) < 1 ? entry.x : entry.x + Math.sign(dx) * radius;
          nextPaths.push({
            id: entry.id,
            d:
              Math.abs(dx) < 1
                ? `M ${entry.x} ${top} V ${busY}`
                : `M ${entry.x} ${top} V ${busY + radius} A ${radius} ${radius} 0 0 ${dx > 0 ? 1 : 0} ${joinX} ${busY}`,
          });
          return joinX;
        });
        const leftJoin = Math.min(endX, ...busJoins);
        const rightJoin = Math.max(endX, ...busJoins);
        if (leftJoin < endX)
          nextPaths.push({ id: 'hub-bus-left', d: `M ${leftJoin} ${busY} H ${endX}` });
        if (rightJoin > endX)
          nextPaths.push({ id: 'hub-bus-right', d: `M ${rightJoin} ${busY} H ${endX}` });
      }
    }

    rows.slice(1).forEach((row, rowIndex) => {
      const parentRow = rows[rowIndex];
      row.forEach((entry) => {
        const parent = parentRow.reduce((closest, candidate) =>
          Math.abs(candidate.x - entry.x) < Math.abs(closest.x - entry.x) ? candidate : closest
        );
        const bridgeY = parent.bottom + (entry.top - parent.bottom) / 2;
        parentById.set(entry.id, parent);
        nextPaths.push({
          id: entry.id,
          d:
            Math.abs(entry.x - parent.x) < 1
              ? `M ${entry.x} ${entry.top - 3} V ${parent.bottom + 3}`
              : `M ${entry.x} ${entry.top - 3} V ${bridgeY} H ${parent.x} V ${parent.bottom + 3}`,
        });
      });
    });

    if (firstRow && routeLineEndY !== null) {
      entries.forEach((entry) => {
        let current = entry;
        let d = `M ${entry.x} ${entry.top - 3}`;
        let parent = parentById.get(current.id);
        while (parent) {
          const bridgeY = parent.bottom + (current.top - parent.bottom) / 2;
          d +=
            Math.abs(current.x - parent.x) < 1
              ? ` V ${parent.bottom + 3}`
              : ` V ${bridgeY} H ${parent.x} V ${parent.bottom + 3}`;
          d += ` V ${parent.top - 3}`;
          current = parent;
          parent = parentById.get(current.id);
        }
        if (firstRow.length === 1) {
          d += ` V ${routeLineEndY}`;
        } else if (routeBusY !== null) {
          d += ` ${routeBranchToBus(current.x, current.top - 3, routeBusY, endX)} V ${routeLineEndY}`;
        }
        nextActivityPaths.push({ id: entry.id, d });
      });
    }
    setCanvasSize({ width: stageBox.width, height: stageBox.height });
    setRoutePaths(nextPaths);
    setActivityPaths(nextActivityPaths);
    setRouteArrow(nextArrow);
  }, []);

  useLayoutEffect(() => {
    const frame = requestAnimationFrame(updatePaths);
    const stage = stageRef.current;
    if (!stage) return () => cancelAnimationFrame(frame);
    const observer = new ResizeObserver(updatePaths);
    observer.observe(stage);
    if (hubRef.current) observer.observe(hubRef.current);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, [selectedModels, nodeColumns, updatePaths]);

  const activityIsVisible =
    routerActivity.candidateId !== null &&
    (routerActivity.active ||
      activityObservedAtMs - routerActivity.updatedAtMs < ROUTER_ACTIVITY_GRACE_MS);
  const activeRoutePath = activityIsVisible
    ? activityPaths.find((path) => path.id === routerActivity.candidateId)
    : undefined;
  const routeRowCount = Math.ceil(selectedModels.length / nodeColumns);
  const stageMinHeight = Math.max(
    550,
    156 +
      HUB_TO_NODE_GAP +
      routeRowCount * 80 +
      Math.max(0, routeRowCount - 1) * NODE_ROW_GAP +
      56 +
      24
  );

  return (
    <div className="free-model-router h-full min-h-[620px] pb-1">
      <div
        ref={stageRef}
        className={`relative overflow-hidden ${routerEnabled ? '' : 'is-disabled'}`}
        style={{ minHeight: `max(100%, ${stageMinHeight}px)` }}
      >
        <svg
          className="absolute inset-0 z-0 h-full w-full pointer-events-none"
          viewBox={`0 0 ${canvasSize.width} ${canvasSize.height}`}
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          {routePaths.map((path) => (
            <path key={path.id} d={path.d} className="free-model-route-path" />
          ))}
          {routerEnabled && activeRoutePath && (
            <path
              key={activeRoutePath.id}
              d={activeRoutePath.d}
              pathLength="1"
              ref={(path) => {
                if (path)
                  path.style.setProperty(
                    '--free-model-route-glow-span',
                    String(Math.min(1, Math.max(0.07, 20 / path.getTotalLength())))
                  );
              }}
              className="free-model-route-glow"
            />
          )}
          {routeArrow && (
            <path
              d={`M ${routeArrow.x} ${routeArrow.y} L ${routeArrow.x - 6} ${routeArrow.y + HUB_ARROW_HEIGHT} H ${routeArrow.x + 6} Z`}
              className="free-model-route-arrow"
            />
          )}
        </svg>

        <RouterHub
          hubRef={hubRef}
          title={t('freeModels.router.title')}
          enabled={routerEnabled}
          baseUrl={routerBaseUrl}
          onCopy={(endpoint) => {
            void copyText(`http://${endpoint}`).then((ok) =>
              showToast(
                ok ? 'success' : 'error',
                t(ok ? 'feedback.step1.copied' : 'feedback.step1.failed')
              )
            );
          }}
        />

        {selectedModels.length === 0 ? (
          <div className="absolute inset-0 z-10 flex items-center justify-center text-center pointer-events-none">
            <div>
              <div className="font-medium text-sm text-cyber-text">
                {t('freeModels.router.emptyTitle')}
              </div>
              <div className="mt-2 text-xs text-cyber-text-muted">
                {t('freeModels.router.emptyDesc')}
              </div>
            </div>
          </div>
        ) : (
          <>
            <DndContext
              sensors={sensors}
              collisionDetection={routeCollisionDetection}
              onDragStart={({ active }) => setActiveDragId(String(active.id))}
              onDragEnd={handleDragEnd}
              onDragCancel={() => setActiveDragId(null)}
            >
              <SortableContext
                items={selectedModels.map((model) => model.id)}
                strategy={rectSortingStrategy}
              >
                <div
                  className="free-model-node-grid relative z-10"
                  style={{
                    gridTemplateColumns: `repeat(${Math.min(selectedModels.length, nodeColumns)}, minmax(0, calc((100% - ${(nodeColumns - 1) * NODE_COLUMN_GAP}px) / ${nodeColumns})))`,
                    columnGap: NODE_COLUMN_GAP,
                    rowGap: NODE_ROW_GAP,
                    paddingInline: NODE_SIDE_PADDING,
                    marginTop: HUB_TO_NODE_GAP,
                  }}
                >
                  {selectedModels.map((model, index) => (
                    <SortableRouteModel
                      key={model.id}
                      model={model}
                      account={accountFor(model)}
                      keyless={keylessFor(model)}
                      providerIconSrc={providerIconFor(model)}
                      priority={index + 1}
                      active={routerEnabled && activeRoutePath?.id === model.id}
                      modelIdOptions={modelIdOptionsFor(model)}
                      {...modelMetadataFor(model)}
                      saving={savingModelIds.has(model.id)}
                      onModelChange={switchModel}
                      registerNode={setNodeRef}
                    />
                  ))}
                </div>
              </SortableContext>
              {createPortal(
                <RouterRemoveDropZone visible={activeDragId !== null} stageRef={stageRef} />,
                document.body
              )}
              {createPortal(
                <DragOverlay
                  dropAnimation={null}
                  zIndex={1002}
                  className="pointer-events-none font-sans text-cyber-text"
                >
                  {activeDragModel && (
                    <RouteModelCard
                      model={activeDragModel}
                      account={accountFor(activeDragModel)}
                      keyless={keylessFor(activeDragModel)}
                      providerIconSrc={providerIconFor(activeDragModel)}
                      compact
                      priority={selectedModels.indexOf(activeDragModel) + 1}
                      overlay
                    />
                  )}
                </DragOverlay>,
                document.body
              )}
            </DndContext>
            <p className="mx-auto mt-14 flex max-w-[680px] flex-wrap items-center justify-center gap-x-2 gap-y-1 cursor-default text-center text-sm leading-6 text-cyber-text-secondary">
              <span>{t('freeModels.router.reorderHint')}</span>
              <span aria-hidden="true" className="text-cyber-text-muted">
                |
              </span>
              <span className="inline-flex items-center gap-1.5">
                <span>{t('freeModels.router.removeHintPrefix')}</span>
                <Trash2 size={14} role="img" aria-label={t('btn.delete')} />
                <span>{t('freeModels.router.removeHintSuffix')}</span>
              </span>
            </p>
          </>
        )}
      </div>
    </div>
  );
}

function KeylessModelLabel({ card = false }: { card?: boolean }) {
  const { t } = useI18n();
  return (
    <span
      className={`shrink-0 whitespace-nowrap font-medium text-cyber-accent ${card ? 'text-xs !leading-5' : 'text-[10px] leading-none'}`}
    >
      {t('freeModels.noApiKey')}
    </span>
  );
}

function FreeModelProviderRow({
  group,
  selected,
  onAdd,
}: {
  group: FreeModelProviderGroup;
  selected: boolean;
  onAdd: () => void;
}) {
  const { t } = useI18n();
  const iconSrc = getModelIcon(group.name, '');
  const keyless = group.models.every(isKeylessFreeModel);
  const hostname = (() => {
    try {
      return new URL(group.baseUrl).hostname;
    } catch {
      return group.baseUrl;
    }
  })();
  const openDocs = () => shellOpen(group.docsUrl).catch(() => window.open(group.docsUrl, '_blank'));

  return (
    <div className="relative flex items-stretch rounded overflow-hidden bg-cyber-surface">
      <button
        type="button"
        onClick={onAdd}
        aria-label={`${t('freeModels.addToRouter')}: ${group.name}`}
        aria-pressed={selected}
        className="group/left flex-1 min-h-[64px] bg-gradient-to-r from-transparent to-transparent hover:from-cyber-text/15 hover:to-transparent transition-[background-image] duration-200"
      />
      <button
        type="button"
        onClick={openDocs}
        aria-label={`${t('freeModels.docs')}: ${group.name}`}
        className="group/right flex-1 min-h-[64px] bg-gradient-to-l from-transparent to-transparent hover:from-cyber-text/15 hover:to-transparent transition-[background-image] duration-200"
      />

      <div className="pointer-events-none absolute inset-0 flex items-center gap-3 px-3">
        {selected ? (
          <Check
            size={22}
            strokeWidth={2.5}
            className="flex-shrink-0 text-cyber-accent group-hover/left:scale-110 transition-all"
          />
        ) : (
          <Plus
            size={22}
            strokeWidth={2.5}
            className="flex-shrink-0 text-cyber-text-muted group-hover/left:text-cyber-text group-hover/left:scale-110 transition-all"
          />
        )}
        <div className="flex-shrink-0">
          {iconSrc ? (
            <img
              src={iconSrc}
              alt=""
              className="w-6 h-6"
              onError={(event) => {
                (event.target as HTMLImageElement).style.display = 'none';
              }}
            />
          ) : (
            <div className="w-6 h-6 flex items-center justify-center text-cyber-text">
              <Box size={22} />
            </div>
          )}
        </div>
        <div className="flex-1 min-w-0 flex flex-col justify-center">
          <div className="flex min-w-0 items-center gap-2 text-sm font-bold leading-none">
            <span className="min-w-0 truncate">{group.name}</span>
            {keyless && <KeylessModelLabel />}
          </div>
          <div className="text-[10px] text-cyber-text-secondary truncate leading-tight mt-1 opacity-70">
            {hostname}
          </div>
        </div>
        <ExternalLink
          size={18}
          strokeWidth={2.25}
          className="flex-shrink-0 text-cyber-text-muted group-hover/right:text-cyber-text group-hover/right:scale-110 transition-all"
        />
      </div>
    </div>
  );
}

export function FreeModelsPanel() {
  const { t, locale } = useI18n();
  const compactTabs = locale === 'en' || locale === 'ja';
  const { showToast } = useToast();
  const confirm = useConfirm();
  const [activeTab, setActiveTab] = useState<'saved' | 'account' | 'free'>('saved');
  const [accountRiskAcknowledged, setAccountRiskAcknowledged] = useState(() => {
    try {
      return localStorage.getItem(ACCOUNT_PROXY_RISK_ACKNOWLEDGED_KEY) === 'true';
    } catch {
      return false;
    }
  });
  const acknowledgeAccountRisk = (acknowledged: boolean) => {
    setAccountRiskAcknowledged(acknowledged);
    try {
      localStorage.setItem(ACCOUNT_PROXY_RISK_ACKNOWLEDGED_KEY, String(acknowledged));
    } catch {
      // Keep the current choice if local storage is unavailable.
    }
  };
  const [addingId, setAddingId] = useState<string | null>(null);
  const setActivePage = useNavigationStore((state) => state.setActivePage);
  const modelContext = useModelNexus();
  const {
    models,
    customModels,
    selectedIds,
    refreshing,
    scanProvider,
    scanProgress,
    refresh,
    addSelectedModel,
    removeSelectedModel,
  } = useFreeModels();
  const {
    userModels,
    isLoadingModels,
    modelUsageData,
    refreshingUsageIds,
    isRefreshingUsage,
    refreshSingleUsage,
    volcAkSkMissingIds,
    openAkskModal,
    handleCardEdit,
    handleCardDelete,
    setNewModelForm,
    setEditingModelId,
    setModelModalDestination,
    setShowAddModelModal,
    setKeyDestroyed,
    setShowApiKey,
  } = modelContext;
  const providerGroups = useMemo(() => {
    const groups = new Map<string, FreeModelProviderGroup>();
    models.forEach((model) => {
      const id = `${model.providerId}:${model.baseUrl}`;
      const group = groups.get(id);
      if (group) group.models.push(model);
      else {
        groups.set(id, {
          id,
          name: model.provider,
          baseUrl: model.baseUrl,
          docsUrl: model.docsUrl,
          models: [model],
        });
      }
    });
    return [...groups.values()].sort((left, right) => {
      return providerRank(left.models[0]) - providerRank(right.models[0]);
    });
  }, [models]);

  const openProvider = useCallback(
    (group: FreeModelProviderGroup) => {
      const modelIds = group.models.map((model) => model.modelId);
      setNewModelForm({
        name: group.name,
        baseUrl: group.baseUrl,
        anthropicUrl: '',
        apiKey: '',
        modelId: modelIds[0] ?? '',
        modelIdOptions: modelIds,
        freeModelIds: group.models
          .filter((model) => model.freeType === 'perpetual' || isKeylessFreeModel(model))
          .map((model) => model.modelId),
      });
      setEditingModelId(null);
      setModelModalDestination('freeRouter');
      setShowAddModelModal(true);
    },
    [setEditingModelId, setModelModalDestination, setNewModelForm, setShowAddModelModal]
  );

  const openCustomModel = () => {
    setNewModelForm({ name: '', baseUrl: '', anthropicUrl: '', apiKey: '', modelId: '' });
    setEditingModelId(null);
    setKeyDestroyed(false);
    setShowApiKey(false);
    setModelModalDestination('freeRouter');
    setShowAddModelModal(true);
  };

  const addSavedModel = async (model: (typeof userModels)[number]) => {
    if (addingId || selectedIds.has(model.internalId)) return;
    if (selectedIds.size >= api.SMART_ROUTER_CANDIDATE_LIMIT) {
      showToast('warning', t('freeModels.router.limitReached'));
      return;
    }
    setAddingId(model.internalId);
    try {
      await addSelectedModel({
        internalId: model.internalId,
        name: model.name,
        baseUrl: model.baseUrl,
        anthropicUrl: model.anthropicUrl,
        apiKey: model.apiKey,
        modelId: model.modelId ?? '',
        accountSource: model.accountSource,
        scope: model.scope,
      });
    } catch (error) {
      console.error('Add saved model to smart router failed:', error);
      showToast('error', t('freeModels.saved.addFailed'));
    } finally {
      setAddingId(null);
    }
  };

  return (
    <div className="free-model-router h-full min-h-0 flex flex-col">
      <div className="h-10 shrink-0 px-2 mb-2 flex items-center justify-between gap-2">
        <div
          role="tablist"
          className={`view-mode-tabs min-w-0 flex border border-cyber-border rounded-button overflow-hidden ${compactTabs ? 'flex-1' : ''}`}
        >
          {(['saved', 'account', 'free'] as const).map((tab, index, tabs) => (
            <button
              key={tab}
              type="button"
              role="tab"
              id={`router-tab-${tab}`}
              aria-selected={activeTab === tab}
              aria-controls={`router-panel-${tab}`}
              tabIndex={activeTab === tab ? 0 : -1}
              onClick={() => setActiveTab(tab)}
              onKeyDown={(event) => {
                if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
                event.preventDefault();
                const next =
                  event.key === 'Home'
                    ? 0
                    : event.key === 'End'
                      ? tabs.length - 1
                      : (index + (event.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length;
                setActiveTab(tabs[next]);
                const tabButtons =
                  event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>(
                    '[role="tab"]'
                  );
                tabButtons?.[next]?.focus();
              }}
              className={`view-mode-tab min-w-0 whitespace-nowrap py-1.5 transition-[color] ${
                compactTabs ? 'flex-auto px-[2px] text-[12px] font-sans' : 'px-3 text-sm font-mono'
              } ${
                activeTab === tab
                  ? 'bg-cyber-elevated text-cyber-text'
                  : 'text-cyber-text-secondary hover:text-cyber-text'
              }`}
            >
              {t(
                tab === 'saved'
                  ? 'freeModels.tab.saved'
                  : tab === 'account'
                    ? 'freeModels.tab.account'
                    : 'freeModels.tab.free'
              )}
            </button>
          ))}
        </div>
        <button
          type="button"
          disabled={activeTab === 'account' && !accountRiskAcknowledged}
          onClick={() => {
            if (activeTab === 'account') {
              if (accountRiskAcknowledged) openAccountModelForm('codex', modelContext);
            } else openCustomModel();
          }}
          aria-label={t('freeModels.customAdd')}
          className="w-9 h-9 shrink-0 flex items-center justify-center border border-cyber-border rounded-button text-cyber-text-secondary enabled:hover:text-cyber-text enabled:hover:bg-cyber-elevated disabled:opacity-40 transition-colors"
        >
          <Plus size={16} />
        </button>
      </div>
      <div
        role="tabpanel"
        id={`router-panel-${activeTab}`}
        aria-labelledby={`router-tab-${activeTab}`}
        className="flex-1 min-h-0 flex flex-col"
      >
        {activeTab === 'saved' ? (
          <div className="flex-1 px-2 pt-0.5 pb-2 overflow-y-auto">
            {isLoadingModels ? (
              <div className="h-full flex items-center justify-center" aria-busy="true">
                <RefreshCw size={18} className="animate-spin text-cyber-text-muted" />
              </div>
            ) : userModels.length === 0 ? (
              <div className="h-full flex flex-col items-center justify-center gap-3 text-xs">
                <p className="text-cyber-text-muted">{t('freeModels.saved.empty')}</p>
                <button
                  type="button"
                  onClick={() => setActivePage('models')}
                  className="text-cyber-accent hover:underline"
                >
                  {t('freeModels.saved.manage')}
                </button>
              </div>
            ) : (
              <div className="space-y-2">
                {userModels.map((model) => {
                  const selected = selectedIds.has(model.internalId);
                  const adding = addingId === model.internalId;
                  return (
                    <ModelListCard
                      key={model.internalId}
                      model={model}
                      onSelect={() => void addSavedModel(model)}
                      selectionDisabled={selected || Boolean(addingId)}
                      selectionLabel={`${t(selected ? 'freeModels.saved.added' : 'freeModels.addToRouter')}: ${model.name} — ${model.modelId ?? ''}`}
                      selection={
                        adding ? (
                          <RefreshCw
                            size={22}
                            className="shrink-0 animate-spin text-cyber-text-muted"
                          />
                        ) : selected ? (
                          <Check
                            size={22}
                            strokeWidth={2.5}
                            className="shrink-0 text-cyber-accent"
                          />
                        ) : (
                          <Plus
                            size={22}
                            strokeWidth={2.5}
                            className="shrink-0 text-cyber-text-muted"
                          />
                        )
                      }
                      usage={modelUsageData[model.internalId]}
                      refreshing={isRefreshingUsage || refreshingUsageIds.has(model.internalId)}
                      onRefreshUsage={(modelId) =>
                        volcAkSkMissingIds.has(modelId)
                          ? openAkskModal(modelId)
                          : refreshSingleUsage(modelId)
                      }
                      onEditModel={handleCardEdit}
                      onDeleteModel={async (modelId) => {
                        const ok = await confirm({
                          title: t('model.deleteTitle'),
                          message: t('model.deleteConfirm'),
                          confirmText: t('btn.delete'),
                          cancelText: t('btn.cancel'),
                          type: 'danger',
                        });
                        if (!ok) return;
                        try {
                          await handleCardDelete(modelId);
                          if (selectedIds.has(modelId)) await removeSelectedModel(modelId);
                        } catch (error) {
                          console.error('Delete saved router model failed:', error);
                          showToast('error', t('error.requestFailed'));
                        }
                      }}
                      t={t}
                    />
                  );
                })}
              </div>
            )}
          </div>
        ) : activeTab === 'account' ? (
          <AccountProxyPanel
            acknowledged={accountRiskAcknowledged}
            onAcknowledgedChange={acknowledgeAccountRisk}
          />
        ) : (
          <div className="flex-1 min-h-0 flex flex-col overflow-y-auto">
            <div className="h-10 shrink-0 px-2 flex items-center">
              <button
                type="button"
                onClick={() => void refresh()}
                disabled={refreshing}
                className={`flex-1 h-9 px-3 text-[14px] font-semibold border rounded-button transition-colors flex items-center justify-center gap-2 ${
                  !refreshing
                    ? 'border-cyber-border text-cyber-text-secondary hover:text-cyber-text hover:bg-cyber-elevated'
                    : 'border-cyber-border text-cyber-text-muted cursor-not-allowed'
                }`}
              >
                <RefreshCw size={14} className={refreshing ? 'animate-spin' : ''} />
                {t('freeModels.fetch')}
              </button>
            </div>
            {refreshing && (
              <div className="px-2 pb-2">
                <div className="h-1 rounded-full overflow-hidden bg-cyber-border/30">
                  <div
                    className="free-model-scan-progress h-full rounded-full bg-cyber-accent"
                    style={{ width: `${Math.max(scanProgress, 0.04) * 100}%` }}
                  />
                </div>
                <div className="mt-2 text-[11px] text-cyber-text-secondary truncate">
                  {scanProvider ? (
                    <>
                      {t('freeModels.scanning')}
                      {scanProvider}
                    </>
                  ) : (
                    <>
                      {t('freeModels.scanConnecting')}
                      <span className="free-model-scan-dots" aria-hidden="true">
                        <span>.</span>
                        <span>.</span>
                        <span>.</span>
                      </span>
                    </>
                  )}
                </div>
              </div>
            )}
            <div className="flex-1 p-2">
              {providerGroups.length === 0 ? (
                <div className="h-full flex items-center justify-center">
                  {!refreshing && (
                    <div className="text-xs text-cyber-text-muted text-center">
                      {t('freeModels.fetchHint')}
                    </div>
                  )}
                </div>
              ) : (
                <div className="space-y-2">
                  {providerGroups.map((group) => (
                    <FreeModelProviderRow
                      key={group.id}
                      group={group}
                      selected={group.models.some((model) =>
                        customModels.some(
                          (routeModel) =>
                            selectedIds.has(routeModel.internalId) &&
                            routeModel.baseUrl === model.baseUrl &&
                            routeModel.modelId === model.modelId
                        )
                      )}
                      onAdd={() => openProvider(group)}
                    />
                  ))}
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
