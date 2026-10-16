import * as api from '../../api/tauri';
import { useManagedAccounts } from './useManagedAccounts';

export function useKiloAccounts(
  enabled: boolean,
  hasModel: boolean,
  clearModel: () => void,
  showError: (error: string) => void,
  navigationKey: string
) {
  return useManagedAccounts<api.KiloAccount, api.KiloLogin>(
    'kilo',
    enabled,
    hasModel,
    clearModel,
    showError,
    {
      list: api.listKiloAccounts,
      start: api.startKiloLogin,
      poll: api.pollKiloLogin,
      cancel: api.cancelKiloLogin,
      open: (login) => api.openExternal(login.verificationUri),
      remove: (account) => api.deleteKiloAccount(account.id),
      refresh: (account) => api.refreshKiloAccount(account.id),
      label: (account) => account.email,
      pollInterval: 3000,
    },
    navigationKey
  );
}

export function useCopilotAccounts(
  enabled: boolean,
  hasModel: boolean,
  clearModel: () => void,
  showError: (error: string) => void,
  toolId: api.CopilotTool,
  navigationKey: string
) {
  return useManagedAccounts<api.CopilotAccount, api.CopilotLogin>(
    toolId,
    enabled,
    hasModel,
    clearModel,
    showError,
    {
      list: () => api.listCopilotAccounts(toolId),
      start: () => api.startCopilotLogin(toolId),
      poll: api.pollCopilotLogin,
      cancel: api.cancelCopilotLogin,
      remove: (account) => api.deleteCopilotAccount(account.id),
      refresh: (account) => api.refreshCopilotAccount(account.id),
      label: (account) => account.login,
    },
    navigationKey
  );
}
