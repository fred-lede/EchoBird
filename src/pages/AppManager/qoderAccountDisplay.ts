import type { QoderAccount } from '../../api/qoder';
import type { TKey } from '../../i18n';

export function qoderRewardStatus(
  account: Pick<QoderAccount, 'dailyReward'>,
  now = Date.now() / 1000
): 'unknown' | 'claimable' | 'claimed' | 'unavailable' {
  const reward = account.dailyReward;
  return reward &&
    Number.isFinite(reward.checkedAt) &&
    reward.checkedAt > 0 &&
    reward.checkedAt < reward.resetAt &&
    Number.isFinite(reward.resetAt) &&
    reward.resetAt > now
    ? reward.status
    : 'unknown';
}

export function qoderExpiry(value?: number | null, now = Date.now() / 1000): number | undefined {
  if (value == null || !Number.isFinite(value)) return undefined;
  const seconds = value >= 1_000_000_000_000 ? Math.floor(value / 1000) : value;
  return seconds > now && new Date(seconds * 1000).getUTCFullYear() < 9999 ? seconds : undefined;
}

export function qoderSubscriptionExpiry(
  account: Pick<QoderAccount, 'plan' | 'expiresAt'>
): number | undefined {
  return account.plan?.trim().toLowerCase() === 'personal_standard'
    ? undefined
    : qoderExpiry(account.expiresAt);
}

export function qoderQuotaLabel(name: string, t: (key: TKey) => string): string {
  const labels: Record<string, TKey> = {
    Credits: 'agent.baseCredits',
    'Add-on': 'agent.additionalCredits',
    Organization: 'agent.organizationCredits',
    Dedicated: 'agent.personalResourceCredits',
  };
  return Object.prototype.hasOwnProperty.call(labels, name) ? t(labels[name]) : name;
}
