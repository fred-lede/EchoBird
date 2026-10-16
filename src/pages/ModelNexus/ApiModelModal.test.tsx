import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import * as api from '../../api/tauri';
import * as catalogApi from '../../api/models';
import { ModelIdCombobox } from '../../components/ModelIdCombobox';
import type { ModelConfig } from '../../api/types';
import { AddModelModal, ModelNexusProvider } from './ModelNexus';
import { useModelNexus, type NewModelForm } from './context';

const { showToast, addSelectedModel, updateSelectedModel, t } = vi.hoisted(() => ({
  showToast: vi.fn(),
  addSelectedModel: vi.fn(),
  updateSelectedModel: vi.fn(),
  t: (key: string) => key,
}));
vi.mock('../../hooks/useI18n', () => ({ useI18n: () => ({ t }) }));
vi.mock('../../components/Toast', () => ({ useToast: () => ({ showToast }) }));
vi.mock('../../components/ModelIdCombobox', () => ({ ModelIdCombobox: () => null }));
vi.mock('../../components', () => ({
  ModelCard: () => null,
  ModelCardSkeleton: () => null,
  getModelIcon: () => null,
}));
vi.mock('../FreeModels/FreeModels', () => ({
  useFreeModels: () => ({
    addSelectedModel,
    updateSelectedModel,
    selectedIds: new Set(),
  }),
}));
vi.mock('../../api/tauri', () => ({
  getModels: vi.fn(),
  addModel: vi.fn(),
  updateModel: vi.fn(),
  deleteModel: vi.fn(),
  isKeyDestroyed: vi.fn(),
  decryptSecret: vi.fn(),
  encryptSecret: vi.fn(),
  queryModelUsage: vi.fn(),
  testModel: vi.fn(),
  SMART_ROUTER_CANDIDATE_LIMIT: 10,
}));
vi.mock('../../api/models', () => ({
  getCachedUpstreamModelCatalog: vi.fn(),
  fetchUpstreamModelCatalog: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(() => {
    throw new Error('Unexpected native command');
  }),
}));
vi.mock('@tauri-apps/plugin-clipboard-manager', () => ({ readText: vi.fn() }));

const saved: ModelConfig = {
  internalId: 'saved-api-fixture',
  name: 'Saved provider',
  baseUrl: 'https://models.fixture.test/v1',
  apiKey: 'enc:v1:opaque-fixture',
  modelId: 'my-existing-model',
  scope: 'modelCenter',
};
const directory: catalogApi.UpstreamModelCatalog = {
  models: [
    { id: 'provider/Case-Sensitive-ID', free: true },
    { id: 'metered-model', multiplier: 0.75 },
  ],
  requiresApiKey: true,
};
let renderer: ReactTestRenderer | undefined;
let context: ReturnType<typeof useModelNexus>;

function Harness() {
  const state = useModelNexus();
  useLayoutEffect(() => {
    context = state;
  });
  return <AddModelModal />;
}
async function mount() {
  await act(async () => {
    renderer = create(
      <ModelNexusProvider>
        <Harness />
      </ModelNexusProvider>
    );
  });
}
async function openAdd(patch: Partial<NewModelForm> = {}) {
  await act(async () => {
    context.setNewModelForm({
      name: 'New provider',
      baseUrl: saved.baseUrl,
      anthropicUrl: '',
      apiKey: 'fixture-key',
      modelId: 'initial-model',
      modelIdOptions: ['initial-model', 'fallback-model'],
      ...patch,
    });
    context.setEditingModelId(null);
    context.setShowAddModelModal(true);
  });
}
function field() {
  return renderer!.root.findByType(ModelIdCombobox);
}
function button(label: string) {
  return renderer!.root.findAllByType('button').find((node) => node.children.includes(label))!;
}
async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}
function expectNoModelActions() {
  expect(api.addModel).not.toHaveBeenCalled();
  expect(api.updateModel).not.toHaveBeenCalled();
  expect(api.deleteModel).not.toHaveBeenCalled();
  expect(api.queryModelUsage).not.toHaveBeenCalled();
  expect(api.testModel).not.toHaveBeenCalled();
  expect(api.decryptSecret).not.toHaveBeenCalled();
  expect(api.encryptSecret).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
}

beforeEach(() => {
  vi.useFakeTimers();
  renderer = undefined;
  vi.mocked(api.getModels).mockReset().mockResolvedValue([saved]);
  addSelectedModel.mockReset().mockResolvedValue(undefined);
  vi.mocked(api.isKeyDestroyed).mockReset().mockResolvedValue(false);
  vi.mocked(api.updateModel)
    .mockReset()
    .mockResolvedValue({ ...saved, modelId: directory.models[0].id });
  vi.mocked(catalogApi.getCachedUpstreamModelCatalog).mockReset().mockResolvedValue(null);
  vi.mocked(catalogApi.fetchUpstreamModelCatalog).mockReset().mockResolvedValue(directory);
});
afterEach(() => {
  act(() => renderer?.unmount());
  vi.clearAllMocks();
  vi.useRealTimers();
});

it('opens saved API Edit through the shared handler, refreshes manually and writes the selected original ID only on Save', async () => {
  vi.mocked(catalogApi.getCachedUpstreamModelCatalog).mockResolvedValue(directory);
  await mount();
  expect(catalogApi.getCachedUpstreamModelCatalog).not.toHaveBeenCalled();
  await act(async () => context.handleCardEdit(saved));
  expect(context.editingModelId).toBe(saved.internalId);
  expect(context.newModelForm.modelId).toBe(saved.modelId);
  expect(renderer!.root.findByProps({ id: 'model-config-dialog-title' }).children).toEqual([
    'model.editConfig',
  ]);
  expect(field().props.options).toEqual(directory.models.map((model) => model.id));
  expect(field().props.freeModelIds).toEqual(['provider/Case-Sensitive-ID']);
  expect(field().props.optionMultipliers).toEqual({ 'metered-model': 0.75 });
  expect(
    renderer!.root.findAllByType('input').find((node) => node.props.placeholder === 'sk-...')!.props
      .readOnly
  ).toBe(true);
  await advance(5000);
  expect(catalogApi.fetchUpstreamModelCatalog).not.toHaveBeenCalled();
  expectNoModelActions();
  await act(async () => field().props.onRefresh());
  expect(catalogApi.fetchUpstreamModelCatalog).toHaveBeenCalledExactlyOnceWith({
    baseUrl: saved.baseUrl,
    anthropicUrl: '',
    apiKey: saved.apiKey,
  });
  expect(context.newModelForm.modelId).toBe(saved.modelId);
  await act(async () => field().props.onChange('provider/Case-Sensitive-ID'));
  expectNoModelActions();
  await act(async () => button('model.enterSave').props.onClick());
  expect(api.updateModel).toHaveBeenCalledExactlyOnceWith(saved.internalId, {
    name: saved.name,
    baseUrl: saved.baseUrl,
    anthropicUrl: '',
    apiKey: saved.apiKey,
    modelId: 'provider/Case-Sensitive-ID',
    accountSource: undefined,
  });
  expect(context.userModels.find((model) => model.internalId === saved.internalId)?.modelId).toBe(
    'provider/Case-Sensitive-ID'
  );
  expect(updateSelectedModel).toHaveBeenCalledWith(
    expect.objectContaining({
      internalId: saved.internalId,
      modelId: 'provider/Case-Sensitive-ID',
    })
  );
  expect(context.showAddModelModal).toBe(false);
  expect(api.queryModelUsage).not.toHaveBeenCalled();
  expect(api.testModel).not.toHaveBeenCalled();
  expect(api.decryptSecret).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
});

it('automatically refreshes an explicitly opened provider Add and debounces changes from its API key input', async () => {
  await mount();
  await advance(2000);
  expect(catalogApi.fetchUpstreamModelCatalog).not.toHaveBeenCalled();
  await openAdd();
  expect(field().props.options).toEqual(['initial-model', 'fallback-model']);
  expect(catalogApi.getCachedUpstreamModelCatalog).toHaveBeenCalledExactlyOnceWith({
    baseUrl: saved.baseUrl,
    anthropicUrl: '',
    apiKey: 'fixture-key',
  });
  await advance(799);
  expect(catalogApi.fetchUpstreamModelCatalog).not.toHaveBeenCalled();
  await advance(1);
  expect(field().props.options).toEqual(directory.models.map((model) => model.id));
  const keyInput = renderer!.root
    .findAllByType('input')
    .find((node) => node.props.placeholder === 'sk-...')!;
  await act(async () => keyInput.props.onChange({ target: { value: 'replacement-key' } }));
  await advance(799);
  expect(catalogApi.fetchUpstreamModelCatalog).toHaveBeenCalledOnce();
  await advance(1);
  expect(catalogApi.fetchUpstreamModelCatalog).toHaveBeenCalledTimes(2);
  expect(catalogApi.fetchUpstreamModelCatalog).toHaveBeenLastCalledWith({
    baseUrl: saved.baseUrl,
    anthropicUrl: '',
    apiKey: 'replacement-key',
  });
  expect(context.newModelForm.modelId).toBe('initial-model');
  expectNoModelActions();
  await act(async () => button('model.escCancel').props.onClick());
  await advance(200);
  expect(context.showAddModelModal).toBe(false);
  expectNoModelActions();
  expect(showToast).not.toHaveBeenCalled();
});

it('adds a new API route with the configuration returned by Save, without looking it up before selection', async () => {
  const created: ModelConfig = {
    ...saved,
    internalId: 'new-api-route',
    baseUrl: 'https://opencode.ai/inference/v1',
    anthropicUrl: 'https://models.fixture.test/anthropic',
    modelId: 'cat-jam-free',
    apiKey: 'enc:v1:opaque-from-save',
    scope: 'smartRouter',
  };
  vi.mocked(api.addModel).mockResolvedValue(created);
  await mount();
  await openAdd({ baseUrl: created.baseUrl, apiKey: '', modelId: created.modelId! });
  await act(async () => context.setModelModalDestination('freeRouter'));
  await act(async () => button('model.enterSave').props.onClick());
  expect(addSelectedModel).toHaveBeenCalledExactlyOnceWith({
    internalId: created.internalId,
    name: created.name,
    baseUrl: created.baseUrl,
    anthropicUrl: created.anthropicUrl,
    apiKey: created.apiKey,
    modelId: created.modelId,
    accountSource: undefined,
    scope: 'smartRouter',
  });
  expect(api.deleteModel).not.toHaveBeenCalled();
  expect(api.decryptSecret).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
  expect(context.showAddModelModal).toBe(false);
});

it('deactivates model fetching immediately on Cancel and ignores success during the closing animation', async () => {
  let resolve!: (value: catalogApi.UpstreamModelCatalog) => void;
  vi.mocked(catalogApi.fetchUpstreamModelCatalog).mockReturnValue(
    new Promise((done) => {
      resolve = done;
    })
  );
  await mount();
  await openAdd({ modelId: '' });
  await advance(800);
  expect(field().props.refreshing).toBe(true);
  await act(async () => button('model.escCancel').props.onClick());
  expect(context.modelModalAnimatingOut).toBe(true);
  expect(context.showAddModelModal).toBe(true);
  expect(field().props.disabled).toBe(true);
  const closingForm = context.newModelForm;
  await act(async () => resolve(directory));
  expect(context.newModelForm).toBe(closingForm);
  expect(context.newModelForm.modelId).toBe('');
  expectNoModelActions();
  expect(showToast).not.toHaveBeenCalled();
  await advance(2000);
  expect(context.showAddModelModal).toBe(false);
  expect(catalogApi.fetchUpstreamModelCatalog).toHaveBeenCalledOnce();
});
