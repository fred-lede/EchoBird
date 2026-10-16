import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  cacheAccountProxyModels,
  getAccountProxyModels,
  getCachedAccountProxyModels,
  listProxyAccounts,
} from './accountProxy';
import type { AccountSource } from './types';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

let saved: Map<string, string>;
beforeEach(() => {
  saved = new Map();
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => saved.get(key) ?? null,
    setItem: (key: string, value: string) => saved.set(key, value),
  });
});
afterEach(() => {
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

it.each(['trae', 'traecn', 'kiro'] as const)(
  'reads saved %s accounts and model choices using the exact saved source',
  async (provider) => {
    const source: AccountSource = { provider, accountId: 'shared-id' };
    const account = { source, label: 'Saved identity', plan: 'Pro' };
    vi.mocked(invoke).mockResolvedValueOnce([account]).mockResolvedValueOnce(['model-one']);
    expect(await listProxyAccounts(provider)).toEqual([account]);
    expect(invoke).toHaveBeenNthCalledWith(1, 'list_proxy_accounts', { provider });
    expect(await getAccountProxyModels(source)).toEqual(['model-one']);
    expect(invoke).toHaveBeenNthCalledWith(2, 'get_account_proxy_models', { source });
    expect(invoke).toHaveBeenCalledTimes(2);
  }
);

it('isolates both regional directories for the same account ID, including a cached empty list', () => {
  const global: AccountSource = { provider: 'trae', accountId: 'shared-id' };
  const cn: AccountSource = { provider: 'traecn', accountId: 'shared-id' };
  expect(getCachedAccountProxyModels(global)).toBeUndefined();
  expect(getCachedAccountProxyModels(cn)).toBeUndefined();
  cacheAccountProxyModels(global, ['global-model']);
  cacheAccountProxyModels(cn, ['cn-model']);
  expect(getCachedAccountProxyModels(global)).toEqual(['global-model']);
  expect(getCachedAccountProxyModels(cn)).toEqual(['cn-model']);
  cacheAccountProxyModels(cn, []);
  expect(getCachedAccountProxyModels(cn)).toEqual([]);
  expect(getCachedAccountProxyModels(global)).toEqual(['global-model']);
  expect([...saved.values()].sort()).toEqual(['["global-model"]', '[]'].sort());
  expect(invoke).not.toHaveBeenCalled();
});
