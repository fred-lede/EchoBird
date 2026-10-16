// Local conversation history. Model context is persisted separately by the backend.
import { useState, useRef, useEffect, useCallback } from 'react';
import type { BubbleChip } from '../components/chat/ChatBubble';

export interface DiskMsg {
  role: string;
  content: string;
  chips?: BubbleChip[];
}

export interface ChatConversation<T> {
  id: string;
  serverId: string;
  sessionKey: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messages: T[];
}

interface ConversationState<T> {
  activeConversationId: string;
  conversations: ChatConversation<T>[];
}

export const CONVERSATIONS_STORAGE_KEY = 'echobird:agent:conversations';
const LEGACY_PREFIX = 'echobird:chat:agent_';

function newConversation<T>(serverId: string): ChatConversation<T> {
  const id = crypto.randomUUID();
  return {
    id,
    serverId,
    sessionKey: id,
    title: '',
    createdAt: Date.now(),
    updatedAt: Date.now(),
    messages: [],
  };
}

function readMessages(value: unknown): DiskMsg[] {
  return Array.isArray(value)
    ? value.filter(
        (m): m is DiskMsg => typeof m?.role === 'string' && typeof m?.content === 'string'
      )
    : [];
}

function loadConversations<T>(fromDisk: (m: DiskMsg) => T): ConversationState<T> {
  const conversations: ChatConversation<T>[] = [];
  try {
    const raw = localStorage.getItem(CONVERSATIONS_STORAGE_KEY);
    if (raw) {
      const saved = JSON.parse(raw);
      if (saved.version === 1 && Array.isArray(saved.conversations)) {
        for (const c of saved.conversations) {
          if (
            typeof c?.id !== 'string' ||
            typeof c.serverId !== 'string' ||
            typeof c.sessionKey !== 'string' ||
            typeof c.title !== 'string' ||
            !Number.isFinite(c.createdAt) ||
            !Number.isFinite(c.updatedAt)
          )
            continue;
          conversations.push({ ...c, messages: readMessages(c.messages).map(fromDisk) });
        }
        if (conversations.length) {
          const active =
            conversations.find((c) => c.id === saved.activeConversationId) ??
            [...conversations].sort((a, b) => b.updatedAt - a.updatedAt)[0];
          return { conversations, activeConversationId: active.id };
        }
      }
    }
  } catch {
    // A damaged index must not prevent the old history from being recovered.
  }
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key?.startsWith(LEGACY_PREFIX)) continue;
      try {
        const diskMessages = readMessages(JSON.parse(localStorage.getItem(key) || '[]'));
        if (!diskMessages.length) continue;
        const serverId = key.slice(LEGACY_PREFIX.length);
        conversations.push({
          id: `legacy-${serverId}`,
          serverId,
          sessionKey: serverId,
          title:
            diskMessages
              .find((m) => m.role === 'user')
              ?.content.trim()
              .split('\n')[0]
              .slice(0, 32) || '',
          createdAt: Date.now(),
          updatedAt: Date.now(),
          messages: diskMessages.map(fromDisk),
        });
      } catch {
        // Recover other servers even if one old history is damaged.
      }
    }
  } catch {
    // Storage can be unavailable; keep the conversation usable in memory.
  }
  let active = conversations.find((c) => c.serverId === 'local');
  if (!active) {
    active = newConversation<T>('local');
    conversations.push(active);
  }
  return { conversations, activeConversationId: active.id };
}

export function useChatPersistence<T>({
  toDisk,
  fromDisk,
  debounceMs = 800,
}: {
  toDisk: (msg: T) => DiskMsg | null;
  fromDisk: (msg: DiskMsg) => T;
  debounceMs?: number;
}) {
  const [state, setState] = useState(() => loadConversations(fromDisk));
  const stateRef = useRef(state);
  const dirtyRef = useRef(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const writeStored = useCallback(
    (value: ConversationState<T>) => {
      localStorage.setItem(
        CONVERSATIONS_STORAGE_KEY,
        JSON.stringify({
          version: 1,
          ...value,
          conversations: value.conversations.map((c) => ({
            ...c,
            messages: c.messages.map(toDisk).filter((m) => m !== null),
          })),
        })
      );
    },
    [toDisk]
  );

  const flush = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = null;
    if (!dirtyRef.current) return;
    try {
      writeStored(stateRef.current);
      dirtyRef.current = false;
    } catch {
      // Preserve cached rows and retry on the next change; no blocking dialog.
    }
  }, [writeStored]);

  const update = useCallback((next: ConversationState<T>) => {
    stateRef.current = next;
    dirtyRef.current = true;
    setState(next);
  }, []);

  useEffect(() => {
    if (dirtyRef.current) timerRef.current = setTimeout(flush, debounceMs);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [state, debounceMs, flush]);

  useEffect(() => {
    window.addEventListener('pagehide', flush);
    window.addEventListener('beforeunload', flush);
    return () => {
      window.removeEventListener('pagehide', flush);
      window.removeEventListener('beforeunload', flush);
      flush();
    };
  }, [flush]);

  const setMessages = useCallback(
    (value: T[] | ((prev: T[]) => T[])) => {
      const prev = stateRef.current;
      const active = prev.conversations.find((c) => c.id === prev.activeConversationId)!;
      const messages = typeof value === 'function' ? value(active.messages) : value;
      if (messages === active.messages) return;
      let title = '';
      for (const message of messages) {
        const diskMessage = toDisk(message);
        if (diskMessage?.role !== 'user') continue;
        title = diskMessage.content.trim().split('\n')[0].slice(0, 32);
        break;
      }
      update({
        ...prev,
        conversations: prev.conversations.map((c) =>
          c.id === active.id ? { ...c, messages, title, updatedAt: Date.now() } : c
        ),
      });
    },
    [toDisk, update]
  );

  const selectConversation = useCallback(
    (id: string) => {
      const prev = stateRef.current;
      if (id === prev.activeConversationId || !prev.conversations.some((c) => c.id === id)) return;
      flush();
      update({ ...prev, activeConversationId: id });
      flush();
    },
    [flush, update]
  );

  const createConversation = useCallback(
    (serverId: string) => {
      flush();
      const c = newConversation<T>(serverId);
      update({ activeConversationId: c.id, conversations: [c, ...stateRef.current.conversations] });
      flush();
    },
    [flush, update]
  );

  const selectServer = useCallback(
    (serverId: string) => {
      const existing = stateRef.current.conversations
        .filter((c) => c.serverId === serverId)
        .sort((a, b) => b.updatedAt - a.updatedAt)[0];
      if (existing) selectConversation(existing.id);
      else createConversation(serverId);
    },
    [selectConversation, createConversation]
  );

  const deleteConversation = useCallback(
    async (id: string, cleanup?: () => Promise<void>) => {
      const prev = stateRef.current;
      const deleted = prev.conversations.find((c) => c.id === id);
      if (!deleted) return;
      flush();
      const conversations = prev.conversations.filter((c) => c.id !== id);
      let activeConversationId = prev.activeConversationId;
      if (id === activeConversationId) {
        let active = conversations
          .filter((c) => c.serverId === deleted.serverId)
          .sort((a, b) => b.updatedAt - a.updatedAt)[0];
        if (!active) {
          active = newConversation<T>(deleted.serverId);
          conversations.unshift(active);
        }
        activeConversationId = active.id;
      }
      const next = { conversations, activeConversationId };
      writeStored(next);
      if (cleanup) {
        try {
          await cleanup();
        } catch (error) {
          try {
            writeStored(prev);
          } catch {
            dirtyRef.current = true;
          }
          throw error;
        }
      }
      update(next);
      dirtyRef.current = false;
    },
    [flush, writeStored, update]
  );

  const activeConversation = state.conversations.find((c) => c.id === state.activeConversationId)!;
  return {
    activeConversation,
    activeConversationId: state.activeConversationId,
    messages: activeConversation.messages,
    conversations: state.conversations
      .filter((c) => c.serverId === activeConversation.serverId)
      .sort((a, b) => b.updatedAt - a.updatedAt),
    setMessages,
    selectConversation,
    createConversation,
    selectServer,
    deleteConversation,
  };
}
