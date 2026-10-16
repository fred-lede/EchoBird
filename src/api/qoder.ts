import { invoke } from '@tauri-apps/api/core';

export type QoderEdition = 'qoder' | 'qodercn';
export type QoderTool = 'qoder' | 'qodercn';
export interface QoderAccount {
  id: string;
  edition: QoderEdition;
  name: string;
  email: string | null;
  plan: string | null;
  active: boolean;
  remaining: number | null;
  total: number | null;
  expiresAt: number | null;
  dailyReward?: {
    status: 'claimable' | 'claimed' | 'unavailable';
    checkedAt: number;
    resetAt: number;
    verified?: boolean;
  } | null;
  quotaWindows: {
    name: string;
    total: number | null;
    remaining: number | null;
    expiresAt?: number | null;
  }[];
}
export interface QoderLogin {
  loginId: string;
  verificationUri: string;
  expiresAt: number;
}

export const listQoderAccounts = (edition: QoderEdition) =>
  invoke<QoderAccount[]>('list_qoder_accounts', { edition });
export const startQoderLogin = (edition: QoderEdition) =>
  invoke<QoderLogin>('start_qoder_login', { edition });
export const pollQoderLogin = (loginId: string) =>
  invoke<QoderAccount | null>('poll_qoder_login', { loginId });
export const cancelQoderLogin = (loginId: string) =>
  invoke<void>('cancel_qoder_login', { loginId });
export const refreshQoderAccountQuota = (edition: QoderEdition, accountId: string) =>
  invoke<QoderAccount>('refresh_qoder_account_quota', { edition, accountId });
export const claimQoderDailyCredits = (edition: QoderEdition, accountId: string) =>
  invoke<QoderAccount>('claim_qoder_daily_credits', { edition, accountId });
export const deleteQoderAccount = (edition: QoderEdition, accountId: string) =>
  invoke<void>('delete_qoder_account', { edition, accountId });
export const switchQoderAccount = (edition: QoderEdition, accountId: string, toolId: QoderTool) =>
  invoke<QoderAccount>('switch_qoder_account', { edition, accountId, toolId });
