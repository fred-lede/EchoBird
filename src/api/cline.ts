import { invoke } from '@tauri-apps/api/core';

export type ClineTool = 'cline' | 'clinedesktop';

export interface ClineAccount {
  id: string;
  email: string;
  name?: string | null;
  active: boolean;
  plan?: string | null;
  balanceMicros?: number | null;
  subscriptionEndAt?: number | null;
}

export interface ClineLogin {
  loginId: string;
  verificationUri: string;
  userCode: string;
  expiresAt: number;
}

export const listClineAccounts = () => invoke<ClineAccount[]>('list_cline_accounts');
export const startClineLogin = () => invoke<ClineLogin>('start_cline_login');
export const pollClineLogin = (loginId: string) =>
  invoke<ClineAccount | null>('poll_cline_login', { loginId });
export const cancelClineLogin = (loginId: string) =>
  invoke<void>('cancel_cline_login', { loginId });
export const refreshClineAccount = (accountId: string) =>
  invoke<ClineAccount>('refresh_cline_account', { accountId });
export const deleteClineAccount = (accountId: string) =>
  invoke<void>('delete_cline_account', { accountId });
export const switchClineAccount = (accountId: string, toolId: ClineTool) =>
  invoke<ClineAccount>('switch_cline_account', { accountId, toolId });
