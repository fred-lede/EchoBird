import { invoke } from '@tauri-apps/api/core';

export interface KiloAccount {
  id: string;
  email: string;
  active: boolean;
  plan: string | null;
  balance: number | null;
}
export interface KiloLogin {
  loginId: string;
  verificationUri: string;
  expiresAt: number;
}
export type KiloTool = 'kilo' | 'kilodesktop';
export const listKiloAccounts = () => invoke<KiloAccount[]>('list_kilo_accounts');
export const startKiloLogin = () => invoke<KiloLogin>('start_kilo_login');
export const pollKiloLogin = (loginId: string) =>
  invoke<KiloAccount | null>('poll_kilo_login', { loginId });
export const cancelKiloLogin = (loginId: string) => invoke<void>('cancel_kilo_login', { loginId });
export const switchKiloAccount = (accountId: string, toolId: KiloTool) =>
  invoke<KiloAccount>('switch_kilo_account', { accountId, toolId });
export const refreshKiloAccount = (accountId: string) =>
  invoke<KiloAccount>('refresh_kilo_account', { accountId });
export const deleteKiloAccount = (accountId: string) =>
  invoke<void>('delete_kilo_account', { accountId });

export interface CopilotAccount {
  id: string;
  login: string;
  host: string;
  plan: string | null;
  remaining: number | null;
  total: number | null;
  resetAt: number | null;
  active: boolean;
}
export interface CopilotLogin {
  loginId: string;
  expiresAt: number;
}
export type CopilotTool = 'copilot' | 'copilotdesktop';
export const listCopilotAccounts = (toolId?: CopilotTool) =>
  invoke<CopilotAccount[]>('list_copilot_accounts', { toolId });
export const startCopilotLogin = (toolId: CopilotTool) =>
  invoke<CopilotLogin>('start_copilot_login', { toolId });
export const pollCopilotLogin = (loginId: string) =>
  invoke<CopilotAccount | null>('poll_copilot_login', { loginId });
export const cancelCopilotLogin = (loginId: string) =>
  invoke<void>('cancel_copilot_login', { loginId });
export const switchCopilotAccount = (accountId: string, toolId: CopilotTool) =>
  invoke<CopilotAccount>('switch_copilot_account', { accountId, toolId });
export const refreshCopilotAccount = (accountId: string) =>
  invoke<CopilotAccount>('refresh_copilot_account', { accountId });
export const deleteCopilotAccount = (accountId: string) =>
  invoke<void>('delete_copilot_account', { accountId });
