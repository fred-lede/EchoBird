import React from 'react';
import { useAppManager } from './context';
import { AccountSectionButton, AccountSectionRow } from './AccountSectionPrimitives';
import { QuotaCountdown } from './QuotaCountdown';

export const CursorAccountSection: React.FC<{ tool: 'cursor' | 'grokbot' }> = ({ tool }) => {
  const { cursorAccounts, grokBotAccounts, isLaunching } = useAppManager();
  const {
    accounts,
    selectedId,
    select,
    busy,
    remainingSeconds,
    refreshing,
    authorizationFailedIds,
    refresh,
    add,
    remove,
  } = tool === 'cursor' ? cursorAccounts : grokBotAccounts;
  return (
    <section>
      <AccountSectionButton
        iconSrc={tool === 'cursor' ? '/icons/tools/cursor.svg' : '/icons/tools/grokbot.png'}
        busy={busy}
        disabled={isLaunching}
        remainingSeconds={remainingSeconds}
        onClick={() => void add()}
        onCancel={(tool === 'cursor' ? cursorAccounts : grokBotAccounts).cancelLogin}
      />
      <div className="space-y-2">
        {accounts.map((account) => (
          <AccountSectionRow
            key={account.id}
            selected={selectedId === account.id}
            email={account.email}
            plan={
              account.usage?.plan?.trim().toLowerCase() === 'grok bot plan'
                ? null
                : account.usage?.plan
            }
            refreshing={refreshing.has(account.id)}
            authorizationFailed={authorizationFailedIds.has(account.id)}
            onRefresh={() => void refresh(account)}
            onSelect={() => select(account.id)}
            onDelete={() => void remove(account)}
            secondary={
              account.usage?.remainingPercent == null ? undefined : (
                <span className="flex h-[16px] items-center justify-between">
                  <span
                    role="progressbar"
                    aria-label={account.email}
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={account.usage?.remainingPercent ?? undefined}
                    aria-valuetext={account.usage?.remainingPercent == null ? '—' : undefined}
                    className="h-1.5 min-w-[56px] max-w-[80px] flex-1 overflow-hidden rounded-full bg-cyber-border"
                  >
                    <span
                      className="quota-progress-fill block h-full rounded-full bg-cyber-bg"
                      style={{ width: `${account.usage?.remainingPercent ?? 0}%` }}
                    />
                  </span>
                  <span className="ml-[6px] w-[30px] flex-shrink-0 text-right text-[12px] font-semibold leading-[16px] text-cyber-text">
                    {account.usage?.remainingPercent == null
                      ? '—'
                      : `${Math.round(account.usage.remainingPercent)}%`}
                  </span>
                  <QuotaCountdown resetAt={account.usage?.resetAt} />
                </span>
              )
            }
          />
        ))}
      </div>
    </section>
  );
};
