// Model Nexus Page — Model cards, debug console, add/edit modal
// Extracted from App.tsx with Provider pattern for shared state

import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { open as shellOpen } from '@tauri-apps/plugin-shell';
import { readText as readClipboardText } from '@tauri-apps/plugin-clipboard-manager';
import {
  DndContext,
  PointerSensor,
  KeyboardSensor,
  closestCenter,
  DragOverlay,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  rectSortingStrategy,
  sortableKeyboardCoordinates,
  arrayMove,
} from '@dnd-kit/sortable';
import { X, Lock, Unlock, RefreshCw, GripVertical } from 'lucide-react';
import { DirectoryRow } from '../../components/DirectoryRow';
import { SortableCard } from '../../components/SortableCard';
import { ModelCard, ModelCardSkeleton, getModelIcon } from '../../components';
import { ViewModeTabs } from '../../components/ViewModeTabs';
import { useToast } from '../../components/Toast';
import { useI18n } from '../../hooks/useI18n';
import * as api from '../../api/tauri';
import type { ModelConfig } from '../../api/types';
import { normalizeAnthropicUrl, normalizeOpenaiUrl } from '../../utils/normalizeUrl';
import { ModelNexusContext, useModelNexus } from './context';
import type { NewModelForm } from './context';
import type { ModelUsageData } from '../../api/tauri';
import modelDirectory from '../../data/modelDirectory.json';
import { useFreeModels } from '../FreeModels/FreeModels';
import { AccountModelFields } from './AccountModelFields';
import { ApiModelFields } from './ApiModelFields';

// ===== Provider =====

/** Whether a model's endpoint is Volcengine (cn) - needs AK/SK for usage.
 *  Mirrors the backend can_handle + URL-selection (baseUrl if non-empty, else anthropicUrl). */
const isVolcengineUrl = (baseUrl: string, anthropicUrl?: string | null) => {
  try {
    const url = new URL(baseUrl || anthropicUrl || '');
    return (
      url.protocol === 'https:' &&
      url.hostname === 'ark.cn-beijing.volces.com' &&
      !url.username &&
      !url.password &&
      !url.port &&
      /^\/api\/coding(?:\/v[13])?$/.test(url.pathname.replace(/\/+$/, ''))
    );
  } catch {
    return false;
  }
};

const isZhipuCnUrl = (baseUrl: string, anthropicUrl?: string | null) => {
  try {
    const url = new URL(baseUrl || anthropicUrl || '');
    return url.protocol === 'https:' && url.hostname === 'open.bigmodel.cn';
  } catch {
    return false;
  }
};

const visibleModelNexusModels = (models: ModelConfig[]) =>
  models.filter(
    (model) => model.internalId !== 'local-server' && model.internalId !== 'smart-router'
  );

const isValidModelBaseUrl = (value: string) => {
  try {
    const url = new URL(value);
    return (url.protocol === 'http:' || url.protocol === 'https:') && Boolean(url.host);
  } catch {
    return false;
  }
};

export function ModelNexusProvider({ children }: { children: React.ReactNode }) {
  const { t } = useI18n();
  // Models state
  const [userModels, setUserModels] = useState<ModelConfig[]>([]);
  const [isLoadingModels, setIsLoadingModels] = useState(true);
  const [selectedModel, setSelectedModel] = useState<string | null>('gpt4o');
  const [pingingModelIds, setPingingModelIds] = useState<Set<string>>(new Set());

  // View mode state
  const [viewMode, setViewMode] = useState<'config' | 'usage'>('config');
  const [modelUsageData, setModelUsageData] = useState<Record<string, ModelUsageData>>({});
  const usageAccessRevision = useRef<Record<string, number>>({});
  const editingUsageModel = useRef<ModelConfig | null>(null);
  const usageBatchPending = useRef(false);
  const usageRequests = useRef(new Map<string, number>());
  const [isRefreshingUsage, setIsRefreshingUsage] = useState(false);
  const [refreshingUsageIds, setRefreshingUsageIds] = useState<Set<string>>(new Set());

  const invalidateModelUsage = useCallback((modelId: string) => {
    usageAccessRevision.current[modelId] = (usageAccessRevision.current[modelId] ?? 0) + 1;
    setModelUsageData((previous) => {
      const next = { ...previous };
      delete next[modelId];
      return next;
    });
    setRefreshingUsageIds((previous) => {
      const next = new Set(previous);
      next.delete(modelId);
      return next;
    });
  }, []);

  const updateUserModel = (updated: ModelConfig, form: NewModelForm) => {
    const previous = editingUsageModel.current;
    if (
      !previous ||
      previous.internalId !== updated.internalId ||
      previous.apiKey !== form.apiKey ||
      previous.baseUrl !== form.baseUrl ||
      (previous.anthropicUrl || '') !== form.anthropicUrl
    ) {
      invalidateModelUsage(updated.internalId);
    }
    setUserModels((models) =>
      models.map((model) => (model.internalId === updated.internalId ? updated : model))
    );
  };
  // Volcengine AK/SK (per-model: one account per model)
  const { showToast } = useToast();
  const [volcAkSkMissingIds, setVolcAkSkMissingIds] = useState<Set<string>>(new Set());
  const [volcAkSkModelId, setVolcAkSkModelId] = useState<string | null>(null);
  const [zhipuTeamModal, setZhipuTeamModal] = useState<{
    modelId: string;
    access: api.ZhipuTeamAccess | null;
  } | null>(null);

  // Modal state
  const [showAddModelModal, setShowAddModelModal] = useState(false);
  const [modelModalDestination, setModelModalDestination] = useState<'modelNexus' | 'freeRouter'>(
    'modelNexus'
  );
  const [modelModalAnimatingOut, setModelModalAnimatingOut] = useState(false);
  const [editingModelId, setEditingModelId] = useState<string | null>(null);
  const [showApiKey, setShowApiKey] = useState(false);
  const [keyDestroyed, setKeyDestroyed] = useState(false);
  const [newModelForm, setNewModelForm] = useState<NewModelForm>({
    name: '',
    baseUrl: '',
    anthropicUrl: '',
    apiKey: '',
    modelId: '',
  });

  const closeModelModal = useCallback(() => {
    setModelModalAnimatingOut(true);
    setTimeout(() => {
      setModelModalAnimatingOut(false);
      setShowAddModelModal(false);
      setEditingModelId(null);
      editingUsageModel.current = null;
      setModelModalDestination('modelNexus');
    }, 200);
  }, []);

  // Pre-fill values for the AK/SK modal (fetched when opening for a model).
  const [volcAkSkInitial, setVolcAkSkInitial] = useState<{
    access_key: string;
    secret_key: string;
  } | null>(null);

  const openAkskModal = async (internalId: string) => {
    try {
      setVolcAkSkInitial(await api.getVolcAksk(internalId));
    } catch {
      setVolcAkSkInitial(null);
    }
    setVolcAkSkModelId(internalId);
  };

  const openZhipuTeamModal = async (modelId: string) => {
    try {
      const access = await api.getZhipuTeamAccess(modelId);
      setZhipuTeamModal({ modelId, access });
    } catch {
      showToast('error', t('model.quota.accessFailed'));
    }
  };

  const handleCardEdit = useCallback(
    async (model: (typeof userModels)[0]) => {
      // Reload fresh model data from disk to get latest apiKey state
      let freshModel = model;
      try {
        const freshModels = await api.getModels();
        const found = freshModels.find((m) => m.internalId === model.internalId);
        if (found) {
          freshModel = found;
          // Also update the models list with fresh data
          setUserModels(visibleModelNexusModels(freshModels));
        }
      } catch {
        /* fallback to stale model */
      }

      editingUsageModel.current = freshModel;
      setModelModalDestination('modelNexus');
      setEditingModelId(freshModel.internalId);
      if (freshModel.apiKey?.startsWith('enc:v1:') && api.isKeyDestroyed) {
        api.isKeyDestroyed(freshModel.internalId).then((destroyed) => setKeyDestroyed(destroyed));
      } else {
        setKeyDestroyed(false);
      }
      setNewModelForm({
        name: freshModel.name,
        baseUrl: freshModel.baseUrl,
        anthropicUrl: freshModel.anthropicUrl || '',
        apiKey: freshModel.apiKey,
        modelId: freshModel.modelId || '',
        accountSource: freshModel.accountSource,
      });
      setShowAddModelModal(true);
    },
    [setEditingModelId, setKeyDestroyed, setNewModelForm, setShowAddModelModal, setUserModels]
  );

  const handleCardDelete = useCallback(
    async (modelId: string) => {
      await api.deleteModel(modelId);
      invalidateModelUsage(modelId);
      setUserModels((prev) => prev.filter((m) => m.internalId !== modelId));
    },
    [setUserModels, invalidateModelUsage]
  );

  // Test state
  const [testInput, setTestInput] = useState('');
  const [testOutput, setTestOutput] = useState<string[]>([]);
  const [isTesting, setIsTesting] = useState(false);
  const [arrowIndex, setArrowIndex] = useState(0);
  const [modelLatencies, setModelLatencies] = useState<Record<string, number>>({});
  const [modelTerminals, setModelTerminals] = useState<
    Record<string, { input: string; output: string[] }>
  >({});
  const [testProtocol, setTestProtocol] = useState<'openai' | 'anthropic'>('openai');
  const testInputRef = useRef<HTMLInputElement>(null!);
  const [inputFocused, setInputFocused] = useState(false);
  const [cursorPos, setCursorPos] = useState(0);

  // Derived
  const selectedModelData = userModels.find((m) => m.internalId === selectedModel);

  // Load models from config
  useEffect(() => {
    const loadModels = async () => {
      setIsLoadingModels(true);
      if (api.getModels) {
        try {
          const models = await api.getModels();
          setUserModels(visibleModelNexusModels(models));
        } catch (error) {
          console.error('Load models failed:', error);
        }
      }
      setIsLoadingModels(false);
    };
    loadModels();
  }, []);

  // Auto-fill Model ID and API Key for local models
  useEffect(() => {
    const isLocal = (url: string) => url.includes('localhost') || url.includes('127.0.0.1');
    const hasLocalUrl = isLocal(newModelForm.baseUrl) || isLocal(newModelForm.anthropicUrl);

    if (hasLocalUrl) {
      setNewModelForm((prev) => {
        const updates: { modelId?: string; apiKey?: string } = {};
        if (!prev.modelId) updates.modelId = 'local-model';
        if (!prev.apiKey) updates.apiKey = 'not-needed';
        return Object.keys(updates).length > 0 ? { ...prev, ...updates } : prev;
      });
    }
  }, [newModelForm.baseUrl, newModelForm.anthropicUrl]);

  // Marquee animation
  useEffect(() => {
    if (!isTesting) return;
    const timer = setInterval(() => {
      setArrowIndex((prev) => (prev + 1) % 4);
    }, 200);
    return () => clearInterval(timer);
  }, [isTesting]);

  // Listen for model selection change - auto restore terminal history and focus
  useEffect(() => {
    if (selectedModel && modelTerminals[selectedModel]) {
      const saved = modelTerminals[selectedModel];
      setTestInput(saved?.input || '');
      setTestOutput(saved?.output || []);
    } else {
      setTestInput('');
      setTestOutput([]);
    }
    testInputRef.current?.focus();
    // Restore terminal history only when the selected model changes; reading the
    // latest modelTerminals here is intentional (no re-run on map updates).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedModel]);

  // Listen for protocol change - focus input
  useEffect(() => {
    testInputRef.current?.focus();
  }, [testProtocol]);

  // ping --all
  const pingAllModels = async () => {
    if (isTesting) return;
    setIsTesting(true);
    const allModels = userModels;
    setPingingModelIds(new Set(allModels.map((m) => m.internalId)));
    for (const model of allModels) {
      try {
        const result = await api.pingModel(model.internalId);
        setPingingModelIds((prev) => {
          const next = new Set(prev);
          next.delete(model.internalId);
          return next;
        });
        // -1 is the agreed-upon "tested and failed" sentinel that ModelCard
        // turns into a red "Error" label. Leaving latency unset would
        // collapse the failure back into "未测试" (never tested), which
        // misleads the user who just watched the ping run.
        setModelLatencies((prev) => ({
          ...prev,
          [model.internalId]: result?.success ? result.latency : -1,
        }));
      } catch {
        setPingingModelIds((prev) => {
          const next = new Set(prev);
          next.delete(model.internalId);
          return next;
        });
        setModelLatencies((prev) => ({ ...prev, [model.internalId]: -1 }));
      }
    }
    setPingingModelIds(new Set());
    setIsTesting(false);
  };

  // Refresh usage for all models
  const refreshAllUsage = useCallback(async () => {
    if (usageBatchPending.current) return;
    const models = userModels.filter(
      (model) =>
        usageRequests.current.get(model.internalId) !==
        (usageAccessRevision.current[model.internalId] ?? 0)
    );
    if (!models.length) return;
    usageBatchPending.current = true;
    setIsRefreshingUsage(true);
    const revisions = models.map((model) => usageAccessRevision.current[model.internalId] ?? 0);
    models.forEach((model, i) => usageRequests.current.set(model.internalId, revisions[i]));
    setRefreshingUsageIds(
      (previous) => new Set([...previous, ...models.map((model) => model.internalId)])
    );
    try {
      // Failed models keep their last data; obsolete configurations cannot publish results.
      const results = await Promise.allSettled(
        models.map(async (model) => api.queryModelUsage(model.internalId))
      );
      setModelUsageData((prev) => {
        const next = { ...prev };
        results.forEach((r, i) => {
          if (
            r.status === 'fulfilled' &&
            r.value.success &&
            r.value.data &&
            revisions[i] === (usageAccessRevision.current[models[i].internalId] ?? 0)
          ) {
            next[models[i].internalId] = r.value.data;
          }
        });
        return next;
      });
      const failed = models.filter(
        (model, i) =>
          revisions[i] === (usageAccessRevision.current[model.internalId] ?? 0) &&
          !(results[i].status === 'fulfilled' && results[i].value.success && results[i].value.data)
      );
      if (failed.length) {
        showToast(
          'error',
          `${t('model.quota.refreshFailed').replace('{n}', String(failed.length))} ${failed.map((model) => model.name).join(', ')}`
        );
      }
    } finally {
      models.forEach((model, i) => {
        if (usageRequests.current.get(model.internalId) === revisions[i]) {
          usageRequests.current.delete(model.internalId);
        }
      });
      setRefreshingUsageIds((previous) => {
        const next = new Set(previous);
        models.forEach((model, i) => {
          if (revisions[i] === (usageAccessRevision.current[model.internalId] ?? 0)) {
            next.delete(model.internalId);
          }
        });
        return next;
      });
      usageBatchPending.current = false;
      setIsRefreshingUsage(false);
    }
  }, [userModels, showToast, t]);

  // Refresh usage for a single model
  const refreshSingleUsage = async (modelId: string) => {
    const model = userModels.find((m) => m.internalId === modelId);
    if (!model) return;

    const revision = usageAccessRevision.current[modelId] ?? 0;
    if (usageRequests.current.get(modelId) === revision) return;
    usageRequests.current.set(modelId, revision);
    setRefreshingUsageIds((prev) => new Set(prev).add(model.internalId));
    try {
      const result = await api.queryModelUsage(model.internalId);
      if (revision !== (usageAccessRevision.current[modelId] ?? 0)) return;
      if (result.error === 'VOLC_AKSK_REQUIRED') {
        setVolcAkSkMissingIds((prev) => new Set(prev).add(model.internalId));
        return;
      }
      if (result.success && result.data) {
        const data = result.data;
        setModelUsageData((prev) => ({
          ...prev,
          [model.internalId]: data,
        }));
        setVolcAkSkMissingIds((prev) => {
          if (!prev.has(model.internalId)) return prev;
          const next = new Set(prev);
          next.delete(model.internalId);
          return next;
        });
      } else if (result.error) {
        showToast('error', result.error);
      }
    } catch (error) {
      if (revision === (usageAccessRevision.current[modelId] ?? 0)) {
        showToast('error', typeof error === 'string' ? error : t('error.requestFailed'));
      }
    } finally {
      if (usageRequests.current.get(modelId) === revision) usageRequests.current.delete(modelId);
      setRefreshingUsageIds((prev) => {
        if (revision !== (usageAccessRevision.current[modelId] ?? 0)) return prev;
        const next = new Set(prev);
        next.delete(model.internalId);
        return next;
      });
    }
  };

  // On mount / when models load, mark which Volcengine models lack AK/SK.
  useEffect(() => {
    const volcModels = userModels.filter((m) => isVolcengineUrl(m.baseUrl, m.anthropicUrl));
    Promise.all(
      volcModels.map(async (m) => {
        try {
          return { id: m.internalId, missing: !(await api.hasVolcAksk(m.internalId)) };
        } catch {
          return { id: m.internalId, missing: true };
        }
      })
    ).then((results) => {
      setVolcAkSkMissingIds(new Set(results.filter((r) => r.missing).map((r) => r.id)));
    });
  }, [userModels]);

  // Save AK/SK for a specific model, then refresh that model's usage.
  const saveVolcAksk = async (internalId: string, accessKey: string, secretKey: string) => {
    try {
      await api.saveVolcAksk(internalId, accessKey, secretKey);
      invalidateModelUsage(internalId);
      setVolcAkSkMissingIds((prev) => {
        const next = new Set(prev);
        next.delete(internalId);
        return next;
      });
      setVolcAkSkModelId(null);
      await refreshSingleUsage(internalId);
    } catch (e) {
      showToast('error', typeof e === 'string' ? e : String(e));
    }
  };

  // Model test function
  const handleTestModel = async () => {
    if (!testInput.trim() || !selectedModel || isTesting) return;

    const prompt = testInput.trim();
    setTestInput('');
    setIsTesting(true);
    testInputRef.current?.blur();

    // Smart protocol selection
    let effectiveProtocol = testProtocol;
    if (selectedModelData) {
      if (!selectedModelData.baseUrl && selectedModelData.anthropicUrl) {
        effectiveProtocol = 'anthropic';
      } else if (selectedModelData.baseUrl && !selectedModelData.anthropicUrl) {
        effectiveProtocol = 'openai';
      }
    }

    setTestOutput((prev) => [
      ...prev,
      `> ${prompt}`,
      `Sending request via ${effectiveProtocol === 'openai' ? 'OpenAI' : 'Anthropic'}...`,
    ]);

    try {
      if (!api.testModel) {
        setTestOutput((prev) => [...prev, 'Test API not available']);
        return;
      }

      const result = await api.testModel(selectedModel, prompt, effectiveProtocol);

      if (result.success) {
        setModelLatencies((prev) => ({ ...prev, [selectedModel]: result.latency }));
        setTestOutput((prev) => [
          ...prev,
          `Response in ${result.latency}ms`,
          result.response || 'No response',
        ]);
        // Reload model list to refresh test status
        if (api.getModels) {
          const updatedModels = await api.getModels();
          setUserModels(visibleModelNexusModels(updatedModels));
        }
      } else {
        // Sentinel -1 so the model card shows "Error" instead of the
        // pre-test "未测试" placeholder — see pingAllModels for the
        // same reasoning.
        setModelLatencies((prev) => ({ ...prev, [selectedModel]: -1 }));
        setTestOutput((prev) =>
          [
            ...prev,
            result.error || 'Unknown error',
            result.latency > 0 ? `(failed after ${result.latency}ms)` : '',
          ].filter(Boolean)
        );
      }
    } catch (error) {
      setModelLatencies((prev) => ({ ...prev, [selectedModel]: -1 }));
      setTestOutput((prev) => [...prev, String(error)]);
    } finally {
      setIsTesting(false);
    }
  };

  return (
    <ModelNexusContext.Provider
      value={{
        userModels,
        setUserModels,
        updateUserModel,
        isLoadingModels,
        selectedModel,
        setSelectedModel,
        selectedModelData,
        viewMode,
        setViewMode,
        modelUsageData,
        isRefreshingUsage,
        refreshingUsageIds,
        setModelUsageData,
        volcAkSkMissingIds,
        setVolcAkSkMissingIds,
        volcAkSkModelId,
        setVolcAkSkModelId,
        saveVolcAksk,
        testInput,
        setTestInput,
        testOutput,
        setTestOutput,
        isTesting,
        arrowIndex,
        testProtocol,
        setTestProtocol,
        modelLatencies,
        pingingModelIds,
        modelTerminals,
        setModelTerminals,
        testInputRef,
        inputFocused,
        setInputFocused,
        cursorPos,
        setCursorPos,
        showAddModelModal,
        setShowAddModelModal,
        modelModalDestination,
        setModelModalDestination,
        modelModalAnimatingOut,
        editingModelId,
        setEditingModelId,
        newModelForm,
        setNewModelForm,
        showApiKey,
        setShowApiKey,
        keyDestroyed,
        setKeyDestroyed,
        closeModelModal,
        pingAllModels,
        refreshAllUsage,
        refreshSingleUsage,
        handleTestModel,
        handleCardEdit,
        handleCardDelete,
        openAkskModal,
        openZhipuTeamModal,
      }}
    >
      {children}
      {volcAkSkModelId && (
        <UsageAccessModal
          onClose={() => setVolcAkSkModelId(null)}
          onSave={(ak, sk) => saveVolcAksk(volcAkSkModelId, ak, sk)}
          initialAk={volcAkSkInitial?.access_key ?? ''}
          initialSk={volcAkSkInitial?.secret_key ?? ''}
        />
      )}
      {zhipuTeamModal && (
        <UsageAccessModal
          key={zhipuTeamModal.modelId}
          team
          onClose={() => setZhipuTeamModal(null)}
          initialAk={zhipuTeamModal.access?.organizationId ?? ''}
          initialSk={zhipuTeamModal.access?.projectId ?? ''}
          onSave={async (organizationId, projectId) => {
            try {
              await api.saveZhipuTeamAccess(
                zhipuTeamModal.modelId,
                organizationId && projectId ? { organizationId, projectId } : null
              );
              invalidateModelUsage(zhipuTeamModal.modelId);
              setZhipuTeamModal(null);
              await refreshSingleUsage(zhipuTeamModal.modelId);
            } catch {
              showToast('error', t('model.quota.accessFailed'));
            }
          }}
        />
      )}
    </ModelNexusContext.Provider>
  );
}

// ===== Title Actions (view mode tabs + ping/refresh button) =====

export function ModelNexusTitleActions() {
  const { t } = useI18n();
  const { viewMode, setViewMode, pingAllModels, refreshAllUsage, isTesting, isRefreshingUsage } =
    useModelNexus();

  return (
    <div className="ml-auto flex-shrink-0 flex items-center gap-3">
      {/* View mode tabs */}
      <ViewModeTabs
        value={viewMode}
        onChange={setViewMode}
        options={[
          { value: 'config', label: t('model.config') },
          { value: 'usage', label: t('model.usage') },
        ]}
      />

      {/* Action button - same height and style as tabs */}
      {viewMode === 'config' ? (
        <button
          onClick={pingAllModels}
          disabled={isTesting}
          className={`flex items-center gap-1.5 text-sm font-mono px-3 py-1.5 border rounded-button transition-colors ${
            !isTesting
              ? 'border-cyber-border text-cyber-text hover:bg-cyber-text/10'
              : 'border-cyber-border text-cyber-text-muted cursor-not-allowed'
          }`}
        >
          <RefreshCw size={13} className={isTesting ? 'animate-spin' : ''} />
          {t('btn.pingAll')}
        </button>
      ) : (
        <button
          onClick={refreshAllUsage}
          disabled={isRefreshingUsage}
          className={`flex items-center gap-1.5 text-sm font-mono px-3 py-1.5 border rounded-button transition-colors ${
            !isRefreshingUsage
              ? 'border-cyber-border text-cyber-text hover:bg-cyber-text/10'
              : 'border-cyber-border text-cyber-text-muted cursor-not-allowed'
          }`}
        >
          <RefreshCw size={13} className={isRefreshingUsage ? 'animate-spin' : ''} />
          {t('btn.refreshUsage')}
        </button>
      )}
    </div>
  );
}

// ===== Main Content (model card grid) =====

// Two-field usage access configuration, mounted fresh for each model.
function UsageAccessModal({
  onClose,
  onSave,
  initialAk = '',
  initialSk = '',
  team = false,
}: {
  onClose: () => void;
  onSave: (accessKey: string, secretKey: string) => Promise<void>;
  initialAk?: string;
  initialSk?: string;
  team?: boolean;
}) {
  const { t } = useI18n();
  const [ak, setAk] = useState(initialAk);
  const [sk, setSk] = useState(initialSk);
  const [saving, setSaving] = useState(false);
  const valid = team ? Boolean(ak.trim()) === Boolean(sk.trim()) : Boolean(ak.trim() && sk.trim());
  const accessUrl = team
    ? 'https://open.bigmodel.cn'
    : 'https://console.volcengine.com/iam/keymanage';

  const handleSave = async () => {
    if (!valid || saving) return;
    setSaving(true);
    try {
      await onSave(ak.trim(), sk.trim());
    } finally {
      setSaving(false);
    }
  };

  const pasteButton = (setter: (v: string) => void) => (
    <button
      type="button"
      onClick={async () => {
        try {
          const text = (await readClipboardText()).trim();
          if (text) setter(text);
        } catch {
          /* clipboard empty / unreadable - no-op */
        }
      }}
      className="absolute right-2 top-1/2 -translate-y-1/2 text-xs text-cyber-text-secondary"
    >
      {t('model.paste')}
    </button>
  );

  const inputClass =
    'w-full bg-cyber-input border border-cyber-border px-2 py-1.5 pr-16 text-xs text-cyber-text font-mono focus:border-cyber-border focus:outline-none rounded-button';

  return (
    <div
      className="fixed inset-0 z-[9998] flex items-center justify-center"
      onKeyDown={(e) => {
        if (e.key === 'Escape') onClose();
      }}
    >
      <div className="absolute inset-0 bg-black/60" onClick={onClose} />
      <div
        className="relative w-[450px] max-w-[90vw] border border-cyber-border/30 bg-cyber-surface shadow-2xl rounded-xl overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="h-px w-full bg-cyber-border" />
        <div className="px-6 pt-5 pb-4 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-cyber-text font-mono text-sm opacity-60">&gt;_</span>
            <span className="text-base font-bold text-cyber-text">{t('model.accessKey')}</span>
          </div>
          <button
            onClick={onClose}
            className="text-cyber-text-secondary hover:text-cyber-text transition-colors"
          >
            <X size={18} />
          </button>
        </div>
        <div className="px-5 pb-5">
          <div className="space-y-4">
            {team && (
              <p className="text-xs text-cyber-text-secondary">{t('model.quota.teamHint')}</p>
            )}
            <div>
              <label className="block text-xs text-cyber-text-secondary mb-1">
                {team ? 'Organization ID' : t('model.akSkAccessKey')}
              </label>
              <div className="relative">
                <input
                  type="text"
                  placeholder={team ? 'Organization ID' : 'AK...'}
                  value={ak}
                  onChange={(e) => setAk(e.target.value)}
                  autoFocus
                  className={inputClass}
                />
                {pasteButton(setAk)}
              </div>
            </div>
            <div>
              <label className="block text-xs text-cyber-text-secondary mb-1">
                {team ? 'Project ID' : t('model.akSkSecretKey')}
              </label>
              <div className="relative">
                <input
                  type="text"
                  placeholder={team ? 'Project ID' : 'SK...'}
                  value={sk}
                  onChange={(e) => setSk(e.target.value)}
                  className={inputClass}
                />
                {pasteButton(setSk)}
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={() => shellOpen(accessUrl).catch(() => window.open(accessUrl, '_blank'))}
            className="block text-xs text-cyber-accent hover:opacity-80 pt-3"
          >
            {accessUrl}
          </button>
          <div className="flex justify-end gap-3 pt-5">
            <button
              className="text-xs font-mono text-cyber-text-secondary hover:text-cyber-text px-3 py-1"
              onClick={onClose}
            >
              [{t('btn.cancel')}]
            </button>
            <button
              className="text-xs font-mono text-cyber-accent hover:opacity-80 px-3 py-1 disabled:opacity-40"
              onClick={handleSave}
              disabled={saving || !valid}
            >
              [{t('btn.save')}]
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

export function ModelNexusMain() {
  const { t } = useI18n();
  const {
    userModels,
    isLoadingModels,
    viewMode,
    modelUsageData,
    setTestOutput: _setTestOutput,
    testProtocol: _testProtocol,
    modelLatencies,
    pingingModelIds,
    modelTerminals: _modelTerminals,
    isTesting: _isTesting,
    editingModelId: _editingModelId,
    setEditingModelId,
    setNewModelForm,
    setShowAddModelModal,
    setUserModels,
    keyDestroyed: _keyDestroyed,
    handleCardEdit,
    handleCardDelete,
    openAkskModal,
    openZhipuTeamModal,
    refreshSingleUsage,
    refreshingUsageIds,
    volcAkSkMissingIds,
  } = useModelNexus();

  // Drag-reorder: pointer (5px activation so plain clicks pass through) +
  // keyboard (a11y). On drop, reorder in place and persist the full visible
  // order — a failed write just reverts on next reload (best-effort).
  // autoScroll is disabled on purpose: the grid lives in an overflow-y-auto
  // container and edge-drag auto-scroll felt janky. A DragOverlay (portal)
  // floats the dragged card above everything, so the scroll container never
  // clips the lifted card or its shadow.
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates })
  );

  const [activeDragId, setActiveDragId] = useState<string | null>(null);
  const [activeDragWidth, setActiveDragWidth] = useState(0);

  const handleDragStart = (event: DragStartEvent) => {
    const id = String(event.active.id);
    setActiveDragId(id);
    // dnd-kit's active.rect is not measured yet at onDragStart (null), so
    // measure the grid node directly for the DragOverlay ghost's width.
    const el = document.querySelector<HTMLElement>(`[data-sortable-card="${id}"]`);
    setActiveDragWidth(el?.offsetWidth ?? 0);
  };

  const handleDragEnd = (event: DragEndEvent) => {
    setActiveDragId(null);
    const activeId = String(event.active.id);
    const overId = String(event.over?.id ?? '');
    if (!overId || activeId === overId) return;
    const oldIndex = userModels.findIndex((m) => m.internalId === activeId);
    const newIndex = userModels.findIndex((m) => m.internalId === overId);
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(userModels, oldIndex, newIndex);
    setUserModels(next);
    api.reorderModels(next.map((m) => m.internalId)).catch((err) => {
      console.error('reorderModels failed:', err);
    });
  };

  const handleDragCancel = () => setActiveDragId(null);

  const activeDragModel = activeDragId
    ? userModels.find((m) => m.internalId === activeDragId)
    : undefined;

  // Model card body, shared by the grid item and the DragOverlay ghost (the
  // ghost is wrapped in pointer-events-none so its interactions are inert).
  const renderModelCard = (model: (typeof userModels)[0]) => {
    const protocols: ('openai' | 'anthropic')[] = [];
    if (model.baseUrl) protocols.push('openai');
    if (model.anthropicUrl) protocols.push('anthropic');
    const isDemo = model.modelType === 'DEMO';
    return (
      <ModelCard
        id={model.internalId}
        name={model.name}
        type={model.modelType || ''}
        baseUrl={model.baseUrl}
        anthropicUrl={model.anthropicUrl}
        modelId={model.modelId || ''}
        protocols={protocols}
        latency={modelLatencies[model.internalId] ?? model.openaiLatency}
        openaiTested={model.openaiTested}
        anthropicTested={model.anthropicTested}
        isPinging={pingingModelIds.has(model.internalId)}
        viewMode={viewMode}
        usageData={modelUsageData[model.internalId]}
        dragHandlePad
        onEdit={isDemo ? undefined : () => handleCardEdit(model)}
        onDelete={isDemo ? undefined : () => handleCardDelete(model.internalId)}
        onRefresh={() => refreshSingleUsage(model.internalId)}
        isRefreshingUsage={refreshingUsageIds.has(model.internalId)}
        onAccessKey={
          isVolcengineUrl(model.baseUrl, model.anthropicUrl)
            ? () => openAkskModal(model.internalId)
            : isZhipuCnUrl(model.baseUrl, model.anthropicUrl)
              ? () => openZhipuTeamModal(model.internalId)
              : undefined
        }
        akSkMissing={volcAkSkMissingIds.has(model.internalId)}
      />
    );
  };

  return (
    <>
      <div className="flex-1 overflow-y-auto">
        <DndContext
          sensors={sensors}
          collisionDetection={closestCenter}
          onDragStart={handleDragStart}
          onDragEnd={handleDragEnd}
          onDragCancel={handleDragCancel}
          autoScroll={false}
        >
          <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
            {/* Show skeleton when loading */}
            {isLoadingModels ? (
              <>
                <ModelCardSkeleton />
                <ModelCardSkeleton />
                <ModelCardSkeleton />
                <ModelCardSkeleton />
              </>
            ) : (
              <>
                {/* User custom models (drag to reorder) */}
                <SortableContext
                  items={userModels.map((m) => m.internalId)}
                  strategy={rectSortingStrategy}
                >
                  {userModels.map((model) => (
                    <SortableCard
                      key={model.internalId}
                      id={model.internalId}
                      dragLabel={t('model.dragSort')}
                    >
                      {(handle) => (
                        <>
                          {renderModelCard(model)}
                          <span className="absolute top-2 left-2 z-10">{handle}</span>
                        </>
                      )}
                    </SortableCard>
                  ))}
                </SortableContext>

                {/* Add new model button */}
                <div
                  className="h-48 border border-dashed border-cyber-border flex flex-col items-center justify-center hover:border-cyber-border cursor-pointer transition-all rounded-card text-cyber-text-secondary hover:text-cyber-text"
                  onClick={() => {
                    setNewModelForm({
                      name: '',
                      baseUrl: '',
                      anthropicUrl: '',
                      apiKey: '',
                      modelId: '',
                    });
                    setEditingModelId(null);
                    setShowAddModelModal(true);
                  }}
                >
                  <span className="font-bold tracking-wider">{t('btn.addModel')}</span>
                  <span className="text-[10px] opacity-60 mt-1">OpenAI / Anthropic API</span>
                </div>
              </>
            )}
          </div>
          {/* Floating drag ghost — rendered in a portal so the scroll
              container never clips the lifted card or its shadow. */}
          <DragOverlay>
            {activeDragModel && (
              <div
                style={{ width: activeDragWidth }}
                className="pointer-events-none relative shadow-2xl"
              >
                {renderModelCard(activeDragModel)}
                <span
                  aria-hidden="true"
                  className="absolute top-2 left-2 z-10 p-0.5 text-cyber-text-muted/60"
                >
                  <GripVertical size={14} />
                </span>
              </div>
            )}
          </DragOverlay>
        </DndContext>
      </div>
    </>
  );
}

// ===== Right Panel (Provider / Relay tabs) =====

// Right-panel Providers + Relays list. Two tiers:
// • Remote-first: `api.getModelDirectory()` hits echobird.ai/api/model-
//   directory/index.json (with backend-side disk cache). Lets us add a
//   vendor or fix a baseUrl without shipping an app release.
// • Bundled fallback: `src/data/modelDirectory.json` shipped in the app
//   binary. Used when both remote and disk-cache are unavailable
//   (offline, first install + firewall, etc.), and as the immediate
//   first paint before the network round-trip lands.
//
// Per-entry fields (name / url / baseUrl / anthropicUrl / modelId /
// region) and ordering rules (zh locale → 'cn' first, others → 'global'
// first) are unchanged. Edit either the remote JSON or the bundled
// JSON; remote wins when both are present.
type DirectoryEntry = {
  name: string;
  url: string;
  baseUrl: string;
  anthropicUrl: string;
  modelId: string;
  // Optional list of model ids this vendor serves on the same endpoint (e.g.
  // Volcengine exposes many). When present (≥2), clicking the row surfaces a
  // dropdown in the Add-Model modal so users pick instead of hand-typing.
  // model id list is the same across the OpenAI/Anthropic endpoints, so no
  // per-protocol split is needed.
  modelIds?: string[];
  region: 'cn' | 'global';
};

const BUNDLED_PROVIDERS: DirectoryEntry[] = modelDirectory.providers as DirectoryEntry[];
const BUNDLED_RELAYS: DirectoryEntry[] = modelDirectory.relays as DirectoryEntry[];

// Locale-aware reorder: zh* surfaces 'cn' entries first; everything else
// surfaces 'global' first. Within each region we keep the curated JSON order
// (Array.prototype.sort is stable in modern engines).
function sortByLocale(list: DirectoryEntry[], locale: string): DirectoryEntry[] {
  const cnFirst = locale.toLowerCase().startsWith('zh');
  const weight = (e: DirectoryEntry) =>
    cnFirst ? (e.region === 'cn' ? 0 : 1) : e.region === 'global' ? 0 : 1;
  return [...list].sort((a, b) => weight(a) - weight(b));
}

function ProviderRow({ entry, onAdd }: { entry: DirectoryEntry; onAdd: () => void }) {
  const iconSrc = getModelIcon(entry.name, '');
  const openSite = () => shellOpen(entry.url).catch(() => window.open(entry.url, '_blank'));
  return (
    <DirectoryRow
      name={entry.name}
      url={entry.url}
      iconSrc={iconSrc}
      onOpen={openSite}
      openLabel={`Open ${entry.name} website`}
      add={{ onClick: onAdd, label: `Add model: ${entry.name}` }}
    />
  );
}

export function ModelNexusPanel() {
  const { t, locale } = useI18n();

  // Bundled JSON paints immediately, remote swaps in if newer content
  // is available. Failure modes (remote down + cache miss): backend
  // returns null, we keep the bundled state forever. No flicker, no
  // blank panel.
  const [providers, setProviders] = useState<DirectoryEntry[]>(BUNDLED_PROVIDERS);
  const [relays, setRelays] = useState<DirectoryEntry[]>(BUNDLED_RELAYS);

  useEffect(() => {
    let cancelled = false;
    api
      .getModelDirectory()
      .then((remote) => {
        if (cancelled || !remote) return;
        setProviders(remote.providers as DirectoryEntry[]);
        setRelays(remote.relays as DirectoryEntry[]);
      })
      .catch(() => {
        /* keep bundled */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // One scrolling list under two headed sections (no tab switcher). Group order
  // flips with the UI language: zh locales put the 中转站 (cn) section on top;
  // other locales surface the providers (global) section first, 中转站 below.
  const sections = useMemo(() => {
    const relaysSorted = sortByLocale(relays, locale);
    const providersSorted = sortByLocale(providers, locale);
    const cnFirst = locale.toLowerCase().startsWith('zh');
    const ordered: { key: string; title: string; entries: DirectoryEntry[] }[] = cnFirst
      ? [
          { key: 'relays', title: t('model.relays'), entries: relaysSorted },
          { key: 'providers', title: t('model.providers'), entries: providersSorted },
        ]
      : [
          { key: 'providers', title: t('model.providers'), entries: providersSorted },
          { key: 'relays', title: t('model.relays'), entries: relaysSorted },
        ];
    // Hide a group the directory doesn't currently provide.
    return ordered.filter((s) => s.entries.length > 0);
  }, [locale, providers, relays, t]);
  const { setNewModelForm, setEditingModelId, setShowAddModelModal } = useModelNexus();

  const handleAddFromEntry = useCallback(
    (entry: DirectoryEntry) => {
      const options = entry.modelIds ?? [];
      setNewModelForm({
        name: entry.name,
        baseUrl: entry.baseUrl,
        anthropicUrl: entry.anthropicUrl,
        apiKey: '',
        // Default to the curated default, else the first listed id, else blank.
        modelId: entry.modelId || options[0] || '',
        modelIdOptions: options,
      });
      setEditingModelId(null);
      setShowAddModelModal(true);
    },
    [setNewModelForm, setEditingModelId, setShowAddModelModal]
  );

  return (
    <div className="flex-1 px-2 pb-2 overflow-y-auto">
      {sections.map((section) => (
        <div key={section.key}>
          <div className="px-1 py-2 flex items-center gap-2">
            <span className="flex-1 h-px bg-cyber-border/60" />
            <span className="text-[11px] font-semibold uppercase tracking-wider text-cyber-text-secondary whitespace-nowrap">
              {section.title}
            </span>
            <span className="flex-1 h-px bg-cyber-border/60" />
          </div>
          <div className="space-y-2">
            {section.entries.map((entry) => (
              <ProviderRow key={entry.name} entry={entry} onAdd={() => handleAddFromEntry(entry)} />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

// ===== Add/Edit Model Modal =====

export function AddModelModal() {
  const { t } = useI18n();
  const { showToast } = useToast();
  const [isSavingModel, setIsSavingModel] = useState(false);
  const { addSelectedModel, updateSelectedModel, selectedIds } = useFreeModels();
  const {
    showAddModelModal,
    modelModalAnimatingOut,
    editingModelId,
    setEditingModelId,
    newModelForm,
    setNewModelForm,
    keyDestroyed,
    closeModelModal,
    setUserModels,
    updateUserModel,
    setShowAddModelModal,
    modelModalDestination,
    setModelModalDestination,
  } = useModelNexus();

  if (!showAddModelModal) return null;

  // One-click paste affordance for the modal's free-text fields. Plain text,
  // no button styling / hover effect — the label never changes. The `normalize`
  // arg lets a URL field run the pasted text through the SAME normalization the
  // typing path uses, so a pasted full endpoint (`/v1/chat/completions`) is
  // trimmed to a clean base instead of silently keeping a doubled path that
  // would 404 every request while manually typed URLs work.
  //
  // These URL fields have NO overlay control to their right (unlike the API
  // key field, which has the encrypt/decrypt lock button at right-2), so the
  // paste label sits flush at the right edge — right-2 — and the input keeps
  // enough right padding (pr-16) to clear the widest locale's "貼り付け" text.
  const pasteButton = (field: 'baseUrl' | 'anthropicUrl', normalize: (v: string) => string) => (
    <button
      type="button"
      onClick={async () => {
        try {
          const text = (await readClipboardText()).trim();
          if (text) {
            const v = normalize(text);
            setNewModelForm((prev) => ({ ...prev, [field]: v }));
          }
        } catch {
          /* clipboard empty / unreadable — no-op */
        }
      }}
      className="absolute right-2 top-1/2 -translate-y-1/2 cursor-pointer text-xs text-cyber-text-secondary"
    >
      {t('model.paste')}
    </button>
  );

  return (
    <div
      className={`fixed inset-0 z-[9998] flex items-center justify-center transition-all duration-200 ${modelModalAnimatingOut ? 'opacity-0' : 'opacity-100'}`}
      onKeyDown={(e) => {
        if (e.key === 'Escape') closeModelModal();
      }}
    >
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/60" onClick={closeModelModal} />

      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="model-config-dialog-title"
        className={`relative w-[450px] max-w-[90vw] border border-cyber-border/30 bg-cyber-surface shadow-2xl rounded-xl overflow-hidden transition-all duration-200 ${modelModalAnimatingOut ? 'scale-95 opacity-0' : 'scale-100 opacity-100'}`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Top accent line */}
        <div className="h-px w-full bg-cyber-border" />

        {/* Header */}
        <div className="px-6 pt-5 pb-4 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-cyber-text font-mono text-sm opacity-60">&gt;_</span>
            <span id="model-config-dialog-title" className="text-base font-bold text-cyber-text">
              {editingModelId
                ? newModelForm.accountSource
                  ? '编辑账号配置'
                  : t('model.editConfig')
                : modelModalDestination === 'freeRouter'
                  ? t(
                      newModelForm.accountSource
                        ? 'freeModels.addAccountToRouter'
                        : 'freeModels.addToRouter'
                    )
                  : t('btn.addModel')}
            </span>
          </div>
          <button
            onClick={closeModelModal}
            aria-label={t('btn.close')}
            className="text-cyber-text-secondary hover:text-cyber-text transition-colors"
          >
            <X size={18} />
          </button>
        </div>

        {/* Form */}
        <div className="px-5 pb-5">
          <div className="space-y-4">
            {!newModelForm.accountSource && (
              <div>
                <label className="block text-xs text-cyber-text-secondary mb-1">
                  {t('model.name')}
                </label>
                <input
                  type="text"
                  placeholder="e.g. My Model"
                  value={newModelForm.name}
                  onChange={(e) => setNewModelForm((prev) => ({ ...prev, name: e.target.value }))}
                  className="w-full bg-cyber-input border border-cyber-border px-2 py-1.5 text-xs text-cyber-text font-mono focus:border-cyber-border focus:outline-none rounded-button"
                />
              </div>
            )}
            {newModelForm.accountSource ? (
              <AccountModelFields
                source={newModelForm.accountSource}
                modelId={newModelForm.modelId}
                modelIdOptions={newModelForm.modelIdOptions}
                setForm={setNewModelForm}
                active={!modelModalAnimatingOut}
              />
            ) : (
              <>
                <div>
                  <label className="block text-xs text-cyber-text-secondary mb-1">
                    {t('model.openaiUrl')}
                  </label>
                  <div className="relative">
                    <input
                      type="text"
                      placeholder="https://x.x.com/v1"
                      value={newModelForm.baseUrl}
                      onChange={(e) =>
                        setNewModelForm((prev) => ({
                          ...prev,
                          baseUrl: normalizeOpenaiUrl(e.target.value),
                        }))
                      }
                      className="w-full bg-cyber-input border border-cyber-border px-2 py-1.5 pr-16 text-xs text-cyber-text font-mono focus:border-cyber-border focus:outline-none rounded-button"
                    />
                    {pasteButton('baseUrl', normalizeOpenaiUrl)}
                  </div>
                </div>
                <div>
                  <label className="block text-xs text-cyber-text-secondary mb-1">
                    {t('model.anthropicUrl')}
                  </label>
                  <div className="relative">
                    <input
                      type="text"
                      placeholder="https://x.x.com/anthropic"
                      value={newModelForm.anthropicUrl}
                      onChange={(e) =>
                        setNewModelForm((prev) => ({
                          ...prev,
                          anthropicUrl: normalizeAnthropicUrl(e.target.value),
                        }))
                      }
                      className="w-full bg-cyber-input border border-cyber-border px-2 py-1.5 pr-16 text-xs text-cyber-text font-mono focus:border-cyber-border focus:outline-none rounded-button"
                    />
                    {pasteButton('anthropicUrl', normalizeAnthropicUrl)}
                  </div>
                </div>
              </>
            )}
            {!newModelForm.accountSource && (
              <ApiModelFields
                key={editingModelId ?? 'new'}
                form={newModelForm}
                setForm={setNewModelForm}
                editing={!!editingModelId}
                active={!modelModalAnimatingOut && !isSavingModel}
              />
            )}
            {!newModelForm.accountSource && (
              <div>
                <label className="block text-xs text-cyber-text-secondary mb-1">
                  {t('model.apiKey')}
                </label>
                <div className="relative">
                  <input
                    type="text"
                    placeholder="sk-..."
                    value={
                      newModelForm.apiKey.startsWith('enc:v1:')
                        ? '•••••••••••••••'
                        : newModelForm.apiKey
                    }
                    onChange={(e) =>
                      setNewModelForm((prev) => ({ ...prev, apiKey: e.target.value }))
                    }
                    className="w-full bg-cyber-input border border-cyber-border px-2 py-1.5 pr-20 text-xs text-cyber-text font-mono focus:border-cyber-border focus:outline-none rounded-button"
                    readOnly={newModelForm.apiKey.startsWith('enc:v1:')}
                  />
                  {/* One-click paste from clipboard — plain text affordance (no
                    border / button styling, no hover effect, label never
                    changes). Shown only while the key is editable (plaintext);
                    hidden once encrypted (the field is read-only then). */}
                  {newModelForm.apiKey !== 'not-needed' &&
                    !newModelForm.apiKey.startsWith('enc:v1:') && (
                      <button
                        type="button"
                        onClick={async () => {
                          try {
                            const text = (await readClipboardText()).trim();
                            if (text) {
                              setNewModelForm((prev) => ({ ...prev, apiKey: text }));
                            }
                          } catch {
                            /* clipboard empty / unreadable — no-op */
                          }
                        }}
                        className="absolute right-8 top-1/2 -translate-y-1/2 cursor-pointer text-xs text-cyber-text-secondary"
                      >
                        {t('model.paste')}
                      </button>
                    )}
                  {newModelForm.apiKey !== 'not-needed' && (
                    <button
                      type="button"
                      disabled={!newModelForm.apiKey}
                      onClick={async () => {
                        if (newModelForm.apiKey.startsWith('enc:v1:')) {
                          try {
                            const plain = await api.decryptSecret(newModelForm.apiKey);
                            const newKey = plain || '';
                            setNewModelForm((prev) => ({ ...prev, apiKey: newKey }));
                            if (editingModelId) {
                              setUserModels((prev) =>
                                prev.map((m) =>
                                  m.internalId === editingModelId ? { ...m, apiKey: newKey } : m
                                )
                              );
                            }
                          } catch {
                            setNewModelForm((prev) => ({ ...prev, apiKey: '' }));
                          }
                        } else {
                          try {
                            const encrypted = await api.encryptSecret(newModelForm.apiKey);
                            setNewModelForm((prev) => ({ ...prev, apiKey: encrypted }));
                            if (editingModelId) {
                              setUserModels((prev) =>
                                prev.map((m) =>
                                  m.internalId === editingModelId ? { ...m, apiKey: encrypted } : m
                                )
                              );
                            }
                          } catch {
                            // stay plaintext on failure
                          }
                        }
                      }}
                      className={`absolute right-2 top-1/2 -translate-y-1/2 transition-colors hover:opacity-80 ${!newModelForm.apiKey ? 'opacity-30 cursor-not-allowed' : ''}`}
                    >
                      {newModelForm.apiKey.startsWith('enc:v1:') ? (
                        <Lock size={14} className="text-cyber-accent" />
                      ) : (
                        <Unlock size={14} className="text-cyber-text/70" />
                      )}
                    </button>
                  )}
                </div>
                {/* Encryption hint — always rendered (locale-aware exact
                  height, no residual space) and toggled via visibility
                  so the form doesn't shift on encrypt / destroy. */}
                <div
                  aria-hidden={!newModelForm.apiKey.startsWith('enc:v1:')}
                  className={`mt-1 text-xs leading-tight ${
                    keyDestroyed ? 'text-red-400' : 'text-cyber-text/60'
                  } ${!newModelForm.apiKey.startsWith('enc:v1:') ? 'invisible' : ''}`}
                >
                  {keyDestroyed ? t('key.destroyed') : t('key.encrypted')}
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Footer buttons */}
        <div className="flex border-t border-cyber-border">
          <button
            onClick={closeModelModal}
            className="flex-1 px-4 py-3 text-[14px] font-semibold text-cyber-text-secondary hover:text-cyber-text hover:bg-cyber-elevated transition-all border-r border-cyber-border"
          >
            {t('model.escCancel')}
          </button>
          <button
            type="button"
            disabled={isSavingModel}
            onClick={async () => {
              if (isSavingModel) return;
              if (
                !newModelForm.accountSource &&
                newModelForm.baseUrl.trim() &&
                !isValidModelBaseUrl(newModelForm.baseUrl)
              ) {
                showToast('warning', t('model.invalidOpenaiUrl'));
                return;
              }
              if (
                newModelForm.accountSource &&
                (!newModelForm.accountSource.accountId || !newModelForm.modelId.trim())
              ) {
                showToast('warning', t('accountProxy.selectRequired'));
                return;
              }
              if (
                modelModalDestination === 'freeRouter' &&
                !editingModelId &&
                selectedIds.size >= api.SMART_ROUTER_CANDIDATE_LIMIT
              ) {
                showToast('warning', t('freeModels.router.limitReached'));
                return;
              }

              setIsSavingModel(true);
              try {
                if (editingModelId) {
                  const updatedModel = await api.updateModel(editingModelId, {
                    name: newModelForm.name,
                    baseUrl: newModelForm.baseUrl,
                    anthropicUrl: newModelForm.anthropicUrl,
                    apiKey: newModelForm.accountSource ? '' : newModelForm.apiKey,
                    modelId: newModelForm.modelId,
                    accountSource: newModelForm.accountSource,
                  });
                  if (updatedModel) {
                    updateSelectedModel({
                      internalId: updatedModel.internalId,
                      name: updatedModel.name,
                      baseUrl: updatedModel.baseUrl,
                      anthropicUrl: updatedModel.anthropicUrl,
                      apiKey: updatedModel.apiKey,
                      modelId: updatedModel.modelId ?? newModelForm.modelId,
                      accountSource: updatedModel.accountSource,
                      scope: updatedModel.scope,
                    });
                    updateUserModel(updatedModel, newModelForm);
                  }
                } else {
                  const newModel = await api.addModel({
                    name: newModelForm.name,
                    baseUrl: newModelForm.baseUrl,
                    anthropicUrl: newModelForm.anthropicUrl || undefined,
                    apiKey: newModelForm.accountSource ? '' : newModelForm.apiKey,
                    modelId: newModelForm.modelId,
                    accountSource: newModelForm.accountSource,
                    scope: modelModalDestination === 'freeRouter' ? 'smartRouter' : 'modelCenter',
                  });
                  if (modelModalDestination === 'freeRouter') {
                    try {
                      await addSelectedModel({
                        internalId: newModel.internalId,
                        name: newModel.name,
                        baseUrl: newModel.baseUrl,
                        anthropicUrl: newModel.anthropicUrl,
                        apiKey: newModel.apiKey,
                        modelId: newModel.modelId ?? newModelForm.modelId,
                        accountSource: newModel.accountSource,
                        scope: newModel.scope,
                      });
                    } catch (error) {
                      await api.deleteModel(newModel.internalId).catch((rollbackError) => {
                        console.error('Rollback smart router model failed:', rollbackError);
                      });
                      throw error;
                    }
                  } else {
                    setUserModels((prev) => [...prev, newModel]);
                  }
                }

                setEditingModelId(null);
                setNewModelForm({
                  name: '',
                  baseUrl: '',
                  anthropicUrl: '',
                  apiKey: '',
                  modelId: '',
                });
                setShowAddModelModal(false);
                setModelModalDestination('modelNexus');
              } catch (error) {
                console.error('Save model failed:', error);
                showToast('error', t('error.requestFailed'));
              } finally {
                setIsSavingModel(false);
              }
            }}
            className={`flex-1 px-4 py-3 text-[14px] font-semibold transition-all ${
              isSavingModel
                ? 'text-cyber-text-muted cursor-not-allowed'
                : 'text-cyber-text hover:bg-cyber-text/10'
            }`}
          >
            {t('model.enterSave')}
          </button>
        </div>
      </div>
    </div>
  );
}
