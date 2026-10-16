import { invoke } from '@tauri-apps/api/core';

export type MinimaxRegion = 'cn' | 'global';
export type MinimaxTool = 'minimaxcode' | 'minimaxdesktop';
export interface MinimaxAccount {
  id: string;
  email: string;
  name?: string | null;
  region: MinimaxRegion;
  active: boolean;
  plan?: string | null;
  expiresAt?: number | null;
  subscriptionEndAt?: number | null;
  dailyReward?: {
    status: 'claimable' | 'claimed' | 'unavailable';
    checkedAt: number;
  } | null;
  quota?: {
    credits?: number | null;
    purchasedCredits?: number | null;
    grantedCredits?: number | null;
    purchasedTotal?: number | null;
    grantedTotal?: number | null;
    purchasedExpiresAt?: number | null;
    grantedExpiresAt?: number | null;
    windows: {
      name: string;
      usedPercent: number;
      resetsAt?: number | null;
      spanSeconds?: number | null;
    }[];
  } | null;
}
export interface MinimaxLogin {
  loginId: string;
  verificationUri: string;
  userCode: string;
  expiresAt: number;
}

export const listMinimaxAccounts = () => invoke<MinimaxAccount[]>('list_minimax_accounts');
export const startMinimaxLogin = (region: MinimaxRegion) =>
  invoke<MinimaxLogin>('start_minimax_login', { region });
export const pollMinimaxLogin = (loginId: string) =>
  invoke<MinimaxAccount | null>('poll_minimax_login', { loginId });
export const cancelMinimaxLogin = (loginId: string) =>
  invoke<void>('cancel_minimax_login', { loginId });
export const refreshMinimaxAccount = (accountId: string) =>
  invoke<MinimaxAccount>('refresh_minimax_account', { accountId });
export const claimMinimaxDailyCredits = (accountId: string) =>
  invoke<MinimaxAccount>('claim_minimax_daily_credits', { accountId });
export const deleteMinimaxAccount = (accountId: string) =>
  invoke<void>('delete_minimax_account', { accountId });
export const switchMinimaxAccount = (accountId: string, toolId: MinimaxTool) =>
  invoke<MinimaxAccount>('switch_minimax_account', { accountId, toolId });
