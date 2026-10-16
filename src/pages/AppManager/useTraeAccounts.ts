import * as api from '../../api/tauri';
import { useManagedAccounts } from './useManagedAccounts';
import { traeClaimUnavailable } from './traeDailyCredits';

export function useTraeAccounts(
  edition: api.TraeEdition,
  enabled: boolean,
  showError: (error: string) => void
) {
  const managed = useManagedAccounts<api.TraeAccount, api.TraeLogin>(
    edition,
    enabled,
    false,
    () => {},
    showError,
    {
      list: () => api.listTraeAccounts(edition),
      start: () => api.startTraeLogin(edition),
      poll: api.pollTraeLogin,
      cancel: api.cancelTraeLogin,
      open: (login) => api.openExternal(login.verificationUri),
      remove: (account) => api.deleteTraeAccount(edition, account.id),
      refresh: (account) => api.refreshTraeAccountQuota(edition, account.id),
      label: (account) => account.name,
    }
  );
  const claimDaily = (account: api.TraeAccount) => {
    if (
      edition !== 'traecn' ||
      account.edition !== edition ||
      traeClaimUnavailable(account, managed.accounts)
    )
      return Promise.resolve();
    return managed.refresh(account, (row) => api.claimTraeDailyCredits(edition, row.id));
  };
  return { ...managed, claimDaily };
}
