// Parasite APIs — Mother Agent's "Connect" mode wraps the installed Claude
// Code CLI as a drop-in alternative to EchoBird's own agent_loop. The
// frontend toggles into Connect mode and streams responses back via
// `parasite_event`. Internal name stays "parasite" since it accurately
// describes the mechanism (we run their binary without their cooperation).
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { ParasiteSendRequest, ParasiteEvent } from './types';

export async function parasiteListInstalled(): Promise<string[]> {
  return invoke('parasite_list_installed');
}

export async function parasiteSendMessage(request: ParasiteSendRequest): Promise<void> {
  return invoke('parasite_send_message', { request });
}

export async function parasiteAbort(agentId: string, conversationId?: string): Promise<boolean> {
  return invoke('parasite_abort', { agentId, conversationId });
}

export async function parasiteReset(agentId: string, conversationId?: string): Promise<void> {
  return invoke('parasite_reset', { agentId, conversationId });
}

export function listenParasiteEvents(handler: (event: ParasiteEvent) => void): Promise<UnlistenFn> {
  return listen<ParasiteEvent>('parasite_event', (e) => handler(e.payload));
}
