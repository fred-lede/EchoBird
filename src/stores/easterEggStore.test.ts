import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const STORAGE_KEY = 'echobird_easter_egg';
let saved: Map<string, string>;
let storage: { getItem: ReturnType<typeof vi.fn>; setItem: ReturnType<typeof vi.fn> };

beforeEach(() => {
  vi.resetModules();
  saved = new Map();
  storage = {
    getItem: vi.fn((key: string) => saved.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => saved.set(key, value)),
  };
  vi.stubGlobal('localStorage', storage);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('shared Easter Egg setting', () => {
  it('defaults to enabled without writing storage', async () => {
    const { useEasterEggStore } = await import('./easterEggStore');
    expect(useEasterEggStore.getState().enabled).toBe(true);
    expect(storage.getItem).toHaveBeenCalledExactlyOnceWith(STORAGE_KEY);
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it.each([
    ['false', false],
    ['true', true],
    ['unexpected', true],
  ])('restores the existing %s flag as %s without rewriting it', async (value, enabled) => {
    saved.set(STORAGE_KEY, value);
    const { useEasterEggStore } = await import('./easterEggStore');
    expect(useEasterEggStore.getState().enabled).toBe(enabled);
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it('notifies consumers immediately and saves both explicit choices using the same key', async () => {
    const { useEasterEggStore } = await import('./easterEggStore');
    const changes: boolean[] = [];
    const unsubscribe = useEasterEggStore.subscribe((state) => changes.push(state.enabled));

    useEasterEggStore.getState().setEnabled(false);
    expect(changes).toEqual([false]);
    expect(useEasterEggStore.getState().enabled).toBe(false);
    expect(storage.setItem).toHaveBeenCalledExactlyOnceWith(STORAGE_KEY, 'false');

    useEasterEggStore.getState().setEnabled(true);
    expect(changes).toEqual([false, true]);
    expect(storage.setItem).toHaveBeenLastCalledWith(STORAGE_KEY, 'true');
    unsubscribe();
  });

  it('restores a saved disabled choice on restart without a passive write', async () => {
    const { useEasterEggStore } = await import('./easterEggStore');
    useEasterEggStore.getState().setEnabled(false);
    storage.setItem.mockClear();
    vi.resetModules();

    const restarted = (await import('./easterEggStore')).useEasterEggStore;
    expect(restarted.getState().enabled).toBe(false);
    expect(storage.setItem).not.toHaveBeenCalled();
  });

  it('defaults safely after a read failure and still disables in memory after a write failure', async () => {
    storage.getItem.mockImplementation(() => {
      throw new Error('storage read unavailable');
    });
    storage.setItem.mockImplementation(() => {
      throw new Error('storage write unavailable');
    });
    const { useEasterEggStore } = await import('./easterEggStore');
    const listener = vi.fn();
    const unsubscribe = useEasterEggStore.subscribe(listener);

    expect(useEasterEggStore.getState().enabled).toBe(true);
    expect(() => useEasterEggStore.getState().setEnabled(false)).not.toThrow();
    expect(useEasterEggStore.getState().enabled).toBe(false);
    expect(listener).toHaveBeenCalledOnce();
    expect(listener.mock.calls[0][0].enabled).toBe(false);
    expect(storage.setItem).toHaveBeenCalledExactlyOnceWith(STORAGE_KEY, 'false');
    unsubscribe();
  });
});
