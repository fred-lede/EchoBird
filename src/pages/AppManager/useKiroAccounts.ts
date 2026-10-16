import * as api from '../../api/tauri';
import { useManagedAccounts } from './useManagedAccounts';

export function useKiroAccounts(
  edition: api.KiroEdition,
  enabled: boolean,
  showError: (error: string) => void
) {
  return useManagedAccounts<api.KiroAccount, api.KiroLogin>(
    'kiro',
    enabled,
    false,
    () => {},
    showError,
    {
      list: () => api.listKiroAccounts(edition),
      start: () => api.startKiroLogin(edition),
      poll: api.pollKiroLogin,
      cancel: api.cancelKiroLogin,
      open: (login) => api.openExternal(login.verificationUri),
      remove: (account) => api.deleteKiroAccount(edition, account.id),
      refresh: (account) => api.refreshKiroAccountQuota(edition, account.id),
      label: (account) => account.name,
    },
    edition
  );
}
