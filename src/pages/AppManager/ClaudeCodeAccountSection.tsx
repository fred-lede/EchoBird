import React from 'react';
import { useAppManager } from './context';
import { ModelSwitchDivider } from './ModelSwitchDivider';
import { AccountSectionButton, AccountSectionRow } from './AccountSectionPrimitives';
import { QuotaCountdown } from './QuotaCountdown';
import { useI18n } from '../../hooks/useI18n';
export const ClaudeCodeAccountSection: React.FC<{ showDivider?: boolean; desktop?: boolean }> = ({
  showDivider = true,
  desktop = false,
}) => {
  const { claudeCodeAccounts, claudeDesktopAccounts, isLaunching } = useAppManager();
  const { t } = useI18n();
  const group = desktop ? claudeDesktopAccounts : claudeCodeAccounts;
  const { accounts, selectedId, select, busy, remainingSeconds, refreshing, add, refresh, remove } =
    group;
  return (
    <section>
      <AccountSectionButton
        iconSrc={desktop ? '/icons/tools/claudedesktop.svg' : '/icons/tools/claudecode.svg'}
        colorClassName="claude-account-pill"
        busy={busy}
        disabled={desktop && isLaunching}
        remainingSeconds={remainingSeconds}
        waitingLabel={
          desktop
            ? t(
                claudeDesktopAccounts.awaitingClientExit
                  ? 'agent.claudeDesktopExitClient'
                  : 'agent.claudeDesktopLoginClient'
              )
            : undefined
        }
        onClick={() => void add()}
        onCancel={group.cancelLogin}
      />
      {accounts.length > 0 && (
        <div className="space-y-2">
          {accounts.map((account) => (
            <AccountSectionRow
              key={account.id}
              colorClassName="claude-account-pill"
              selected={selectedId === account.id}
              email={account.email}
              plan={account.plan}
              refreshing={refreshing.has(account.id)}
              authorizationFailed={group.authorizationFailedIds.has(account.id)}
              onSelect={() => select(account.id)}
              onRefresh={() => void refresh(account)}
              onDelete={() => void remove(account)}
              secondary={
                account.fiveHour == null && account.sevenDay == null ? undefined : (
                  <span className="flex h-[16px] min-w-0 items-center gap-1 overflow-hidden whitespace-nowrap text-[11px] font-semibold leading-[16px] text-cyber-text">
                    {account.fiveHour && (
                      <span className="flex flex-shrink-0 items-center gap-0.5">
                        {`${account.fiveHour.remainingPercent}%`}
                        {account.fiveHour?.resetAt ? (
                          <QuotaCountdown
                            resetAt={account.fiveHour?.resetAt}
                            compact
                            small
                            parenthesized
                          />
                        ) : (
                          <span className="text-[10px]">5h</span>
                        )}
                      </span>
                    )}
                    {account.fiveHour && account.sevenDay && <span aria-hidden="true">·</span>}
                    {account.sevenDay && (
                      <span className="flex flex-shrink-0 items-center gap-0.5">
                        {`${account.sevenDay.remainingPercent}%`}
                        {account.sevenDay?.resetAt ? (
                          <QuotaCountdown
                            resetAt={account.sevenDay?.resetAt}
                            compact
                            small
                            parenthesized
                          />
                        ) : (
                          <span className="text-[10px]">7d</span>
                        )}
                      </span>
                    )}
                  </span>
                )
              }
            />
          ))}
        </div>
      )}
      {showDivider && <ModelSwitchDivider />}
    </section>
  );
};
