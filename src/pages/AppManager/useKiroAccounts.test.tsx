import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import { useKiroAccounts } from './useKiroAccounts';
import { useNavigationStore } from '../../stores/navigationStore';

vi.mock('../../api/tauri', () => ({
  listKiroAccounts: vi.fn(),
  startKiroLogin: vi.fn(),
  pollKiroLogin: vi.fn(),
  cancelKiroLogin: vi.fn().mockResolvedValue(undefined),
  deleteKiroAccount: vi.fn().mockResolvedValue(undefined),
  refreshKiroAccountQuota: vi.fn(),
  openExternal: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../../hooks/useI18n', () => ({ useI18n: () => ({ t: (key: string) => key }) }));
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => async () => true }));

const crew: api.KiroAccount = {
  id: 'saved',
  edition: 'kirocrew',
  name: 'Crew user',
  plan: 'Pro',
  active: true,
  quota: [],
};
const cli: api.KiroAccount = { ...crew, edition: 'kirocli', name: 'CLI user' };
const error = vi.fn();
let state: ReturnType<typeof useKiroAccounts>;
let renderer: ReactTestRenderer;
function Harness({
  edition = 'kirocrew',
  enabled = true,
}: {
  edition?: api.KiroEdition;
  enabled?: boolean;
}) {
  const accounts = useKiroAccounts(edition, enabled, error);
  useLayoutEffect(() => {
    state = accounts;
  });
  return null;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}
async function tick() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1);
  });
}
async function mount(edition: api.KiroEdition = 'kirocrew', enabled = true) {
  act(() => {
    renderer = create(<Harness edition={edition} enabled={enabled} />);
  });
  await tick();
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-10-14T00:00:00Z'));
  vi.clearAllMocks();
  vi.mocked(api.listKiroAccounts)
    .mockReset()
    .mockImplementation(async (edition) => (edition === 'kirocrew' ? [crew] : [cli]));
  useNavigationStore.getState().setActivePage('apps');
});
afterEach(() => {
  act(() => renderer?.unmount());
  vi.useRealTimers();
});

it.each(['kirocrew', 'kirocli'] as const)(
  '%s: loads and selects cached accounts without refreshing or logging in',
  async (edition) => {
    await mount(edition);
    expect(state.accounts[0].edition).toBe(edition);
    expect(state.selectedId).toBe('saved');
    act(() => state.select(null));
    expect(state.selectedId).toBeNull();
    act(() => state.select('saved'));
    expect(state.selectedId).toBe('saved');
    expect(api.listKiroAccounts).toHaveBeenCalledExactlyOnceWith(edition);
    expect(api.refreshKiroAccountQuota).not.toHaveBeenCalled();
    expect(api.startKiroLogin).not.toHaveBeenCalled();
  }
);

it('shares selection and deletion across desktop and CLI navigation', async () => {
  const second = { ...crew, id: 'second', active: false, name: 'second@example.test' };
  let saved = [crew, second];
  vi.mocked(api.listKiroAccounts).mockImplementation(async () => saved);
  vi.mocked(api.deleteKiroAccount).mockImplementation(async (_edition, id) => {
    saved = saved.filter((row) => row.id !== id);
  });
  await mount();
  act(() => state.select(second.id));
  act(() => renderer.update(<Harness edition="kirocli" />));
  await tick();
  expect(state.selectedId).toBe(second.id);
  expect(state.accounts).toEqual([crew, second]);
  await act(async () => {
    await state.remove(second);
  });
  expect(api.deleteKiroAccount).toHaveBeenCalledExactlyOnceWith('kirocli', second.id);
  act(() => renderer.update(<Harness />));
  await tick();
  expect(state.accounts).toEqual([crew]);
  expect(api.refreshKiroAccountQuota).not.toHaveBeenCalled();
  expect(api.startKiroLogin).not.toHaveBeenCalled();
});

it('does not load native data or start operations for an uninstalled client', async () => {
  await mount('kirocrew', false);
  await act(async () => {
    await state.add();
    await state.refresh(crew);
    await state.remove(crew);
  });
  for (const operation of [
    api.listKiroAccounts,
    api.startKiroLogin,
    api.refreshKiroAccountQuota,
    api.deleteKiroAccount,
  ])
    expect(operation).not.toHaveBeenCalled();
});

it('keeps each client cache and suppresses passive errors and late list responses', async () => {
  await mount();
  const stale = deferred<api.KiroAccount[]>();
  vi.mocked(api.listKiroAccounts).mockReturnValueOnce(stale.promise);
  act(() => renderer.update(<Harness edition="kirocli" />));
  await tick();
  vi.mocked(api.listKiroAccounts).mockRejectedValueOnce(new Error('accountError.read'));
  act(() => renderer.update(<Harness />));
  await tick();
  await act(async () => stale.resolve([cli]));
  expect(state.accounts).toEqual([crew]);
  expect(error).not.toHaveBeenCalled();
  expect(api.refreshKiroAccountQuota).not.toHaveBeenCalled();
});

it.each(['leave', 'timeout', 'cancel'] as const)(
  'cancels browser login on %s and ignores a late account',
  async (reason) => {
    await mount();
    const pending = deferred<api.KiroAccount | null>();
    vi.mocked(api.startKiroLogin).mockResolvedValue({
      loginId: 'attempt',
      verificationUri: 'https://app.kiro.dev/signin',
      expiresAt: Date.now() / 1000 + 120,
    });
    vi.mocked(api.pollKiroLogin).mockReturnValueOnce(pending.promise);
    let adding!: Promise<void>;
    await act(async () => {
      adding = state.add();
    });
    expect(state.remainingSeconds).toBe(60);
    expect(api.openExternal).toHaveBeenCalledExactlyOnceWith('https://app.kiro.dev/signin');
    if (reason === 'leave') {
      act(() => renderer.update(<Harness edition="kirocli" />));
      await tick();
    }
    if (reason === 'timeout')
      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000);
      });
    if (reason === 'cancel') act(() => state.cancelLogin());
    await act(async () => {
      pending.resolve({ ...crew, id: 'late' });
      await adding;
    });
    expect(api.cancelKiroLogin).toHaveBeenCalledWith('attempt');
    expect(state.accounts.some((row) => row.id === 'late')).toBe(false);
    expect(state.busy).toBe(false);
    if (reason === 'timeout') expect(error).toHaveBeenCalledWith('accountError.expired');
    else expect(error).not.toHaveBeenCalled();
    expect(api.refreshKiroAccountQuota).not.toHaveBeenCalled();
  }
);

it('saves a successful login and selects it without an additional quota request', async () => {
  await mount();
  const added = { ...crew, id: 'new', active: false };
  vi.mocked(api.startKiroLogin).mockResolvedValue({
    loginId: 'attempt',
    verificationUri: 'https://app.kiro.dev/signin',
    expiresAt: Date.now() / 1000 + 60,
  });
  vi.mocked(api.pollKiroLogin).mockResolvedValueOnce(added);
  vi.mocked(api.listKiroAccounts).mockResolvedValueOnce([crew, added]);
  await act(async () => {
    await state.add();
  });
  expect(state.selectedId).toBe('new');
  expect(state.accounts).toEqual([crew, added]);
  expect(api.refreshKiroAccountQuota).not.toHaveBeenCalled();
});

it('refreshes explicitly with request isolation and preserves rows on an explicit error', async () => {
  await mount('kirocli');
  const pending = deferred<api.KiroAccount>();
  vi.mocked(api.refreshKiroAccountQuota).mockReturnValueOnce(pending.promise);
  let task!: Promise<unknown>;
  act(() => {
    task = state.refresh(cli);
    void state.refresh(cli);
  });
  expect(state.refreshing.has(cli.id)).toBe(true);
  expect(api.refreshKiroAccountQuota).toHaveBeenCalledExactlyOnceWith('kirocli', cli.id);
  await act(async () => {
    pending.reject(new Error('accountError.network'));
    await task;
  });
  expect(error).toHaveBeenCalledWith('accountError.network');
  expect(state.accounts).toEqual([cli]);
  expect(state.refreshing.size).toBe(0);
});

it('deleting an account invalidates an older refresh result', async () => {
  await mount();
  const pending = deferred<api.KiroAccount>();
  vi.mocked(api.refreshKiroAccountQuota).mockReturnValueOnce(pending.promise);
  let task!: Promise<unknown>;
  act(() => {
    task = state.refresh(crew);
  });
  await act(async () => {
    await state.remove(crew);
  });
  await act(async () => {
    pending.resolve({ ...crew, name: 'Late' });
    expect(await task).toBeUndefined();
  });
  expect(state.accounts).toEqual([]);
  expect(api.deleteKiroAccount).toHaveBeenCalledExactlyOnceWith('kirocrew', crew.id);
});
