import { useEffect, useId, useState } from 'react';
import { Check } from 'lucide-react';
import { accountProxyProviders, listProxyAccounts } from '../../api/accountProxy';
import type { AccountProvider } from '../../api/types';
import { useConfirm } from '../../components/ConfirmDialog';
import { useI18n } from '../../hooks/useI18n';
import { useNavigationStore } from '../../stores/navigationStore';
import { useModelNexus } from '../ModelNexus/context';

export function openAccountModelForm(
  provider: AccountProvider,
  context: ReturnType<typeof useModelNexus>
) {
  context.setNewModelForm({
    name: accountProxyProviders.find((p) => p.id === provider)?.name ?? '',
    baseUrl: '',
    anthropicUrl: '',
    apiKey: '',
    modelId: '',
    accountSource: { provider, accountId: '' },
  });
  context.setEditingModelId(null);
  context.setKeyDestroyed(false);
  context.setShowApiKey(false);
  context.setModelModalDestination('freeRouter');
  context.setShowAddModelModal(true);
}

export function AccountProxyPanel({
  acknowledged,
  onAcknowledgedChange,
}: {
  acknowledged: boolean;
  onAcknowledgedChange: (acknowledged: boolean) => void;
}) {
  const context = useModelNexus();
  const { t } = useI18n();
  const confirm = useConfirm();
  const acknowledgementId = useId();
  const activePage = useNavigationStore((state) => state.activePage);
  const [accountCounts, setAccountCounts] = useState<Partial<Record<AccountProvider, number>>>({});

  useEffect(() => {
    if (activePage !== 'freeModels') return;
    let ignore = false;
    void Promise.resolve().then(async () => {
      if (ignore) return;
      const results = await Promise.allSettled(
        accountProxyProviders.map((provider) => listProxyAccounts(provider.id))
      );
      if (ignore) return;
      setAccountCounts((previous) => {
        const counts = { ...previous };
        results.forEach((result, index) => {
          if (result.status === 'fulfilled')
            counts[accountProxyProviders[index].id] = result.value.length;
        });
        return counts;
      });
    });
    return () => {
      ignore = true;
    };
  }, [activePage]);

  return (
    <div className="flex-1 px-2 pt-0.5 pb-2 overflow-y-auto space-y-2">
      <div className="flex items-center justify-between gap-2 pb-1">
        <div className="flex-1 min-w-0 text-xs text-cyber-text-secondary select-none">
          <label htmlFor={acknowledgementId}>{t('accountProxy.riskReadPrefix')}</label>
          <button
            type="button"
            aria-haspopup="dialog"
            onClick={() =>
              void confirm({
                title: t('accountProxy.riskTitle'),
                message: t('accountProxy.riskMessage'),
                type: 'warning',
                confirmText: t('accountProxy.riskDismiss'),
                cancelText: '',
              })
            }
            className="mx-0.5 whitespace-nowrap text-cyber-text underline underline-offset-2 decoration-cyber-text-secondary/70 hover:text-cyber-accent hover:decoration-cyber-accent transition-colors"
          >
            {t('accountProxy.riskNotice')}
          </button>
          <label htmlFor={acknowledgementId}>{t('accountProxy.riskAcceptSuffix')}</label>
        </div>
        <label className="shrink-0 flex items-center">
          <input
            id={acknowledgementId}
            type="checkbox"
            aria-label={`${t('accountProxy.riskReadPrefix')}${t('accountProxy.riskNotice')}${t('accountProxy.riskAcceptSuffix')}`}
            checked={acknowledged}
            onChange={(event) => onAcknowledgedChange(event.target.checked)}
            className="peer sr-only"
          />
          <span
            aria-hidden="true"
            className={`w-[18px] h-[18px] shrink-0 flex items-center justify-center rounded-[5px] border shadow-sm transition-colors duration-150 peer-focus-visible:ring-2 peer-focus-visible:ring-cyber-accent/40 peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-cyber-bg ${
              acknowledged
                ? 'border-cyber-accent bg-cyber-accent text-white'
                : 'border-cyber-text-muted/50 bg-cyber-input'
            }`}
          >
            {acknowledged && <Check size={12} strokeWidth={3} />}
          </span>
        </label>
      </div>
      {accountProxyProviders.map((provider) => (
        <button
          key={provider.id}
          type="button"
          disabled={!acknowledged}
          onClick={() => {
            if (acknowledged) openAccountModelForm(provider.id, context);
          }}
          aria-label={`${t('freeModels.addToRouter')}: ${provider.name}`}
          className="w-full min-h-[64px] flex items-center gap-3 px-3 bg-cyber-surface rounded text-left enabled:hover:bg-cyber-elevated disabled:opacity-40 disabled:grayscale transition-colors"
        >
          <img
            src={`/icons/tools/${provider.icon}`}
            alt=""
            className="w-8 h-8 shrink-0 object-contain"
          />
          <span className="flex-1 min-w-0 text-base font-bold text-cyber-text truncate">
            {provider.name}
          </span>
          {['antigravity', 'claudeCode', 'qoder', 'qoderCn'].includes(provider.id) && (
            <span className="shrink-0 whitespace-nowrap font-medium text-cyber-accent text-[10px] leading-none">
              {t('accountProxy.highRisk')}
            </span>
          )}
          <span className="shrink-0 text-xs text-cyber-text-secondary tabular-nums whitespace-nowrap">
            {accountCounts[provider.id] === undefined
              ? '—'
              : t(
                  accountCounts[provider.id] === 1
                    ? 'accountProxy.accountCountOne'
                    : 'accountProxy.accountCountOther'
                ).replace('{count}', String(accountCounts[provider.id]))}
          </span>
        </button>
      ))}
    </div>
  );
}
