import React, { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  CONVERSATIONS_STORAGE_KEY,
  useChatPersistence,
  type ChatConversation,
  type DiskMsg,
} from './useChatPersistence';

type Message = DiskMsg & { transient?: boolean };
type SavedState = {
  version: number;
  activeConversationId: string;
  conversations: ChatConversation<DiskMsg>[];
};

const toDisk = (message: Message): DiskMsg | null =>
  message.transient
    ? null
    : {
        role: message.role,
        content: message.content,
        ...(message.chips && { chips: message.chips }),
      };
const fromDisk = (message: DiskMsg): Message => ({ ...message });
let state: ReturnType<typeof useChatPersistence<Message>>;
let renderer: ReactTestRenderer | undefined;
let storage: Map<string, string>;
let setItem: ReturnType<typeof vi.fn>;
let nextId: number;

function Harness() {
  const result = useChatPersistence({ toDisk, fromDisk });
  useLayoutEffect(() => {
    state = result;
  });
  return null;
}

function mount() {
  act(() => {
    renderer = create(<Harness />);
  });
}

function unmount() {
  act(() => renderer?.unmount());
  renderer = undefined;
}

function saved(): SavedState {
  return JSON.parse(storage.get(CONVERSATIONS_STORAGE_KEY)!);
}

function conversation(id: string, serverId: string, updatedAt: number): ChatConversation<DiskMsg> {
  return {
    id,
    serverId,
    sessionKey: id,
    title: id,
    createdAt: updatedAt - 1000,
    updatedAt,
    messages: [{ role: 'user', content: id }],
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-10-12T09:00:00Z'));
  storage = new Map();
  nextId = 0;
  setItem = vi.fn((key: string, value: string) => storage.set(key, value));
  vi.stubGlobal('localStorage', {
    get length() {
      return storage.size;
    },
    key: (index: number) => [...storage.keys()][index] ?? null,
    getItem: (key: string) => storage.get(key) ?? null,
    setItem,
    removeItem: (key: string) => storage.delete(key),
  });
  vi.stubGlobal('crypto', { randomUUID: () => `conversation-${++nextId}` });
  vi.stubGlobal('window', { addEventListener: vi.fn(), removeEventListener: vi.fn() });
});

afterEach(() => {
  unmount();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('Mother Agent conversation persistence', () => {
  it('recovers local and SSH legacy histories with their original backend session keys', () => {
    const local = [{ role: 'user', content: 'Local question\nMore detail' }];
    const remote = [{ role: 'assistant', content: 'SSH reply' }];
    storage.set('echobird:chat:agent_local', JSON.stringify(local));
    storage.set('echobird:chat:agent_ssh-production', JSON.stringify(remote));
    storage.set('echobird:chat:agent_broken', '{');
    mount();
    expect(state.activeConversation.serverId).toBe('local');
    expect(state.activeConversation.sessionKey).toBe('local');
    expect(state.messages).toEqual(local);
    expect(state.activeConversation.title).toBe('Local question');
    expect(setItem).not.toHaveBeenCalled();

    act(() => state.selectServer('ssh-production'));
    expect(state.activeConversation.sessionKey).toBe('ssh-production');
    expect(state.messages).toEqual(remote);
    expect(state.conversations).toHaveLength(1);
    expect(saved().conversations).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ serverId: 'local', sessionKey: 'local', messages: local }),
        expect.objectContaining({
          serverId: 'ssh-production',
          sessionKey: 'ssh-production',
          messages: remote,
        }),
      ])
    );
    unmount();
    mount();
    expect(state.activeConversation.serverId).toBe('ssh-production');
    expect(state.activeConversation.sessionKey).toBe('ssh-production');
    expect(state.messages).toEqual(remote);
  });

  it('flushes separate conversations before rapid switching and again on unmount', () => {
    mount();
    const firstId = state.activeConversationId;
    act(() => state.setMessages([{ role: 'user', content: 'First question' }]));
    act(() => {
      vi.advanceTimersByTime(100);
    });
    expect(setItem).not.toHaveBeenCalled();
    act(() => state.createConversation('local'));
    const secondId = state.activeConversationId;
    expect(secondId).not.toBe(firstId);
    expect(state.activeConversation.sessionKey).toBe(secondId);
    expect(state.messages).toEqual([]);
    expect(saved().conversations.find((c) => c.id === firstId)?.messages).toEqual([
      { role: 'user', content: 'First question' },
    ]);

    act(() => state.setMessages([{ role: 'user', content: 'Second question' }]));
    act(() => state.selectConversation(firstId));
    expect(state.messages).toEqual([{ role: 'user', content: 'First question' }]);
    expect(saved().conversations.find((c) => c.id === secondId)?.messages).toEqual([
      { role: 'user', content: 'Second question' },
    ]);
    act(() =>
      state.setMessages((previous) => [...previous, { role: 'assistant', content: 'First reply' }])
    );
    unmount();
    mount();
    expect(state.activeConversationId).toBe(firstId);
    expect(state.messages).toEqual([
      { role: 'user', content: 'First question' },
      { role: 'assistant', content: 'First reply' },
    ]);
    act(() => state.selectConversation(secondId));
    expect(state.messages).toEqual([{ role: 'user', content: 'Second question' }]);
  });

  it('keeps existing update times and history order when a saved conversation is selected', () => {
    const older = conversation('older', 'local', Date.now() - 60_000);
    const newer = conversation('newer', 'local', Date.now());
    storage.set(
      CONVERSATIONS_STORAGE_KEY,
      JSON.stringify({ version: 1, activeConversationId: newer.id, conversations: [older, newer] })
    );
    mount();
    act(() => state.selectConversation(older.id));
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(state.activeConversationId).toBe(older.id);
    expect(state.conversations.map((c) => c.id)).toEqual([newer.id, older.id]);
    expect(saved().conversations).toEqual([older, newer]);
    expect(saved().activeConversationId).toBe(older.id);
  });

  it('keeps server histories isolated and restores the latest conversation for each server', () => {
    mount();
    const localId = state.activeConversationId;
    act(() => state.setMessages([{ role: 'user', content: 'Local work' }]));
    act(() => state.selectServer('ssh-production'));
    const remoteId = state.activeConversationId;
    expect(state.messages).toEqual([]);
    expect(state.activeConversation.serverId).toBe('ssh-production');
    act(() => state.setMessages([{ role: 'user', content: 'Remote work' }]));
    act(() => state.createConversation('ssh-production'));
    const newerRemoteId = state.activeConversationId;
    act(() => {
      vi.advanceTimersByTime(1);
    });
    act(() => state.setMessages([{ role: 'user', content: 'New remote work' }]));
    expect(state.conversations.map((c) => c.id)).toEqual([newerRemoteId, remoteId]);

    act(() => state.selectServer('local'));
    expect(state.activeConversationId).toBe(localId);
    expect(state.messages).toEqual([{ role: 'user', content: 'Local work' }]);
    expect(state.conversations.map((c) => c.id)).toEqual([localId]);
    act(() => state.selectServer('ssh-production'));
    expect(state.activeConversationId).toBe(newerRemoteId);
    expect(state.messages).toEqual([{ role: 'user', content: 'New remote work' }]);
  });

  it('does not write storage during passive empty mounts, timer ticks, or unmount', () => {
    mount();
    expect(state.messages).toEqual([]);
    expect(state.activeConversation.serverId).toBe('local');
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    unmount();
    expect(setItem).not.toHaveBeenCalled();
    expect(storage.size).toBe(0);
  });

  it('falls back from a malformed index and discards invalid legacy messages', () => {
    storage.set(CONVERSATIONS_STORAGE_KEY, '{');
    storage.set(
      'echobird:chat:agent_local',
      JSON.stringify([
        null,
        { role: 'user' },
        { role: 1, content: 'Bad role' },
        { role: 'user', content: 'Recovered' },
        { role: 'assistant', content: 'Saved reply' },
      ])
    );
    mount();
    expect(state.messages).toEqual([
      { role: 'user', content: 'Recovered' },
      { role: 'assistant', content: 'Saved reply' },
    ]);
    expect(state.activeConversation.sessionKey).toBe('local');
    expect(setItem).not.toHaveBeenCalled();
  });

  it('keeps valid indexed conversations and messages when other saved entries are damaged', () => {
    const valid = conversation('valid', 'local', Date.now());
    storage.set(
      CONVERSATIONS_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        activeConversationId: 'missing',
        conversations: [
          null,
          { id: 'broken' },
          { ...valid, messages: [null, { role: 'assistant', content: 12 }, ...valid.messages] },
        ],
      })
    );
    mount();
    expect(state.activeConversationId).toBe('valid');
    expect(state.messages).toEqual(valid.messages);
    expect(state.conversations).toHaveLength(1);
    expect(setItem).not.toHaveBeenCalled();
  });

  it('retains cached messages after a failed save and retries on the next content change', () => {
    setItem.mockImplementationOnce(() => {
      throw new Error('Storage full');
    });
    mount();
    act(() => state.setMessages([{ role: 'user', content: 'Keep this question' }]));
    act(() => {
      vi.advanceTimersByTime(800);
    });
    expect(setItem).toHaveBeenCalledTimes(1);
    expect(storage.has(CONVERSATIONS_STORAGE_KEY)).toBe(false);
    expect(state.messages).toEqual([{ role: 'user', content: 'Keep this question' }]);
    act(() =>
      state.setMessages((previous) => [
        ...previous,
        { role: 'assistant', content: 'Keep this reply' },
      ])
    );
    act(() => {
      vi.advanceTimersByTime(800);
    });
    expect(setItem).toHaveBeenCalledTimes(2);
    expect(saved().conversations[0].messages).toEqual(state.messages);
    unmount();
    mount();
    expect(state.messages).toEqual([
      { role: 'user', content: 'Keep this question' },
      { role: 'assistant', content: 'Keep this reply' },
    ]);
  });

  it('deletes the active legacy conversation without restoring it or changing other records', async () => {
    storage.set(
      'echobird:chat:agent_local',
      JSON.stringify([{ role: 'user', content: 'Legacy question' }])
    );
    mount();
    const firstId = state.activeConversationId;
    act(() => state.createConversation('local'));
    const secondId = state.activeConversationId;
    act(() => state.setMessages([{ role: 'user', content: 'Keep the other conversation' }]));
    act(() => state.selectConversation(firstId));
    await act(async () => state.deleteConversation(firstId));
    expect(state.conversations.some((conversation) => conversation.id === firstId)).toBe(false);
    expect(state.activeConversationId).toBe(secondId);
    unmount();
    mount();
    expect(state.activeConversationId).toBe(secondId);
    expect(state.conversations.some((conversation) => conversation.id === firstId)).toBe(false);
    expect(state.messages).toEqual([{ role: 'user', content: 'Keep the other conversation' }]);
    act(() => state.createConversation('local'));
    expect(state.messages).toEqual([]);
  });

  it('round-trips mapper-supported tool and error records and attachment chips without transient activity', () => {
    const messages: Message[] = [
      { role: 'user', content: 'Inspect this', chips: [{ type: 'file', name: 'report.md' }] },
      {
        role: 'tool_call',
        content: JSON.stringify({ name: 'inspect', status: 'done', output: 'Found it' }),
      },
      { role: 'error', content: 'An earlier request failed' },
      { role: 'assistant', content: 'Finished' },
      { role: 'working', content: 'Thinking', transient: true },
    ];
    mount();
    act(() => state.setMessages(messages));
    unmount();
    mount();
    expect(state.messages).toEqual(messages.slice(0, -1));
    expect(state.activeConversation.title).toBe('Inspect this');
  });

  it('deletes another conversation without changing the active record, its content, or recency', async () => {
    const older = conversation('older', 'local', Date.now() - 60_000);
    const active = conversation('active', 'local', Date.now());
    storage.set(
      CONVERSATIONS_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        activeConversationId: active.id,
        conversations: [older, active],
      })
    );
    mount();
    await act(async () => {
      await state.deleteConversation(older.id);
    });
    expect(state.activeConversationId).toBe(active.id);
    expect(state.activeConversation).toEqual(active);
    expect(saved().conversations).toEqual([active]);
    unmount();
    mount();
    expect(state.conversations).toEqual([active]);
  });

  it('selects the newest remaining record on the same server and replaces the last deleted record with an empty one', async () => {
    const older = conversation('older', 'local', Date.now() - 60_000);
    const newer = conversation('newer', 'local', Date.now());
    const remote = conversation('remote', 'ssh-production', Date.now());
    storage.set(
      CONVERSATIONS_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        activeConversationId: newer.id,
        conversations: [older, newer, remote],
      })
    );
    mount();
    await act(async () => {
      await state.deleteConversation(newer.id);
    });
    expect(state.activeConversationId).toBe(older.id);
    expect(state.activeConversation).toEqual(older);
    await act(async () => {
      await state.deleteConversation(older.id);
    });
    expect(state.activeConversation.serverId).toBe('local');
    expect(state.messages).toEqual([]);
    expect(state.activeConversationId).not.toBe(older.id);
    expect(saved().conversations).toContainEqual(remote);
    unmount();
    mount();
    expect(state.conversations).toHaveLength(1);
    expect(state.messages).toEqual([]);
  });

  it('keeps cached and persisted records when saving a deletion fails', async () => {
    const active = conversation('active', 'local', Date.now());
    storage.set(
      CONVERSATIONS_STORAGE_KEY,
      JSON.stringify({ version: 1, activeConversationId: active.id, conversations: [active] })
    );
    mount();
    setItem.mockImplementationOnce(() => {
      throw new Error('Storage unavailable');
    });
    const cleanup = vi.fn(async () => {});
    await act(async () => {
      await expect(state.deleteConversation(active.id, cleanup)).rejects.toThrow(
        'Storage unavailable'
      );
    });
    expect(cleanup).not.toHaveBeenCalled();
    expect(state.activeConversation).toEqual(active);
    expect(saved().conversations).toEqual([active]);
  });

  it('restores the persisted row when context cleanup fails after the deletion was saved', async () => {
    const active = conversation('active', 'local', Date.now());
    storage.set(
      CONVERSATIONS_STORAGE_KEY,
      JSON.stringify({ version: 1, activeConversationId: active.id, conversations: [active] })
    );
    mount();
    const cleanup = vi.fn(async () => {
      expect(saved().conversations.some((c) => c.id === active.id)).toBe(false);
      throw new Error('Context file locked');
    });
    await act(async () => {
      await expect(state.deleteConversation(active.id, cleanup)).rejects.toThrow(
        'Context file locked'
      );
    });
    expect(cleanup).toHaveBeenCalledOnce();
    expect(state.activeConversation).toEqual(active);
    expect(saved().conversations).toEqual([active]);
    unmount();
    mount();
    expect(state.activeConversation).toEqual(active);
  });
});
