import React from 'react';
import { useAppManager } from './context';
import { AccountSectionButton, AccountSectionRow } from './AccountSectionPrimitives';
import { QuotaCountdown } from './QuotaCountdown';

export const AntigravityAccountSection: React.FC = () => {
  const { antigravityAccounts, isLaunching, selectedTool } = useAppManager();
  const { accounts, selectedId, select, busy, remainingSeconds, refreshing, refresh, add, remove } =
    antigravityAccounts;
  const minimum = (account: (typeof accounts)[number], prefix: string) => {
    const values = account.quotas.filter((quota) => quota.name.startsWith(prefix));
    return values.reduce<(typeof values)[number] | null>(
      (lowest, quota) =>
        !lowest || quota.remainingPercent < lowest.remainingPercent ? quota : lowest,
      null
    );
  };
  return (
    <section>
      <AccountSectionButton
        iconSrc={`/icons/tools/${selectedTool}.png`}
        busy={busy}
        disabled={isLaunching}
        remainingSeconds={remainingSeconds}
        onClick={() => void add()}
        onCancel={antigravityAccounts.cancelLogin}
      />
      <div className="space-y-2">
        {accounts.map((account) => {
          const gemini = minimum(account, 'gemini');
          const claude = minimum(account, 'claude');
          const parts = [
            gemini == null ? null : `Gemini ${Math.round(gemini.remainingPercent)}%`,
            claude == null ? null : `Claude ${Math.round(claude.remainingPercent)}%`,
          ].filter(Boolean);
          const hasResetTime = gemini?.resetAt || claude?.resetAt;
          const sharedResetAt =
            gemini?.resetAt && gemini.resetAt === claude?.resetAt ? gemini.resetAt : null;
          return (
            <AccountSectionRow
              key={account.id}
              selected={selectedId === account.id}
              email={account.email}
              plan={account.plan}
              secondary={
                hasResetTime ? (
                  <span className="flex min-w-0 items-center gap-1 overflow-hidden whitespace-nowrap text-[11px] leading-[16px]">
                    {gemini && (
                      <span>
                        G {Math.round(gemini.remainingPercent)}%
                        {!sharedResetAt && (
                          <>
                            {' '}
                            <QuotaCountdown resetAt={gemini.resetAt} compact small />
                          </>
                        )}
                      </span>
                    )}
                    {gemini && claude && <span>·</span>}
                    {claude && (
                      <span>
                        C {Math.round(claude.remainingPercent)}%
                        {!sharedResetAt && (
                          <>
                            {' '}
                            <QuotaCountdown resetAt={claude.resetAt} compact small />
                          </>
                        )}
                      </span>
                    )}
                    {sharedResetAt && (
                      <span>
                        · <QuotaCountdown resetAt={sharedResetAt} compact small />
                      </span>
                    )}
                  </span>
                ) : parts.length ? (
                  parts.join(' · ')
                ) : undefined
              }
              refreshing={refreshing.has(account.id)}
              authorizationFailed={antigravityAccounts.authorizationFailedIds.has(account.id)}
              onRefresh={() => void refresh(account)}
              onSelect={() => select(account.id)}
              onDelete={() => void remove(account)}
            />
          );
        })}
      </div>
    </section>
  );
};
