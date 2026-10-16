import { useState } from 'react';
import * as api from '../../api/tauri';
import { useManagedAccounts } from './useManagedAccounts';
import { minimaxRewardStatus } from './minimaxAccountDisplay';

export function useMinimaxAccounts(
  enabled: boolean,
  hasModel: boolean,
  clearModel: () => void,
  showError: (error: string) => void,
  navigationKey: string
) {
  const [region, setRegion] = useState<api.MinimaxRegion>('cn');
  const managed = useManagedAccounts<api.MinimaxAccount, api.MinimaxLogin>(
    'minimax',
    enabled,
    hasModel,
    clearModel,
    showError,
    {
      list: () => api.listMinimaxAccounts(),
      start: () => api.startMinimaxLogin(region),
      open: (login) => api.openExternal(login.verificationUri),
      poll: (id) => api.pollMinimaxLogin(id),
      cancel: (id) => api.cancelMinimaxLogin(id),
      remove: (account) => api.deleteMinimaxAccount(account.id),
      refresh: (account) => api.refreshMinimaxAccount(account.id),
      label: (account) => account.email,
    },
    navigationKey
  );
  const claimDaily = (account: api.MinimaxAccount) => {
    const current = managed.accounts.find((row) => row.id === account.id);
    if (!current || current.region !== account.region) return Promise.resolve();
    const status = minimaxRewardStatus(current);
    if (status === 'claimed' || status === 'unavailable') return Promise.resolve();
    return managed.refresh(current, (row) => api.claimMinimaxDailyCredits(row.id));
  };
  return {
    ...managed,
    claimDaily,
    region,
    setRegion: (value: api.MinimaxRegion) => {
      if (!managed.busy) setRegion(value);
    },
  };
}
