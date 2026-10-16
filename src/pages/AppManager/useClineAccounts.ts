import * as api from '../../api/tauri';
import { useManagedAccounts } from './useManagedAccounts';

export function useClineAccounts(
  enabled: boolean,
  hasModel: boolean,
  clearModel: () => void,
  showError: (error: string) => void,
  navigationKey: string
) {
  return useManagedAccounts<api.ClineAccount, api.ClineLogin>(
    'cline',
    enabled,
    hasModel,
    clearModel,
    showError,
    {
      list: api.listClineAccounts,
      start: api.startClineLogin,
      poll: api.pollClineLogin,
      cancel: api.cancelClineLogin,
      open: (login) => api.openExternal(login.verificationUri),
      refresh: (account) => api.refreshClineAccount(account.id),
      remove: (account) => api.deleteClineAccount(account.id),
      label: (account) => account.email || account.name || account.id,
    },
    navigationKey
  );
}
