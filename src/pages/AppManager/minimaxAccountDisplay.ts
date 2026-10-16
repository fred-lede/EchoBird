import type { MinimaxAccount } from '../../api/minimax';
import type { TKey } from '../../i18n';

export const MINIMAX_REWARD_CACHE_SECONDS = 300;

export function minimaxCreditBuckets(quota: MinimaxAccount['quota']): {
  label: TKey;
  remaining?: number | null;
  total?: number | null;
  expiresAt?: number | null;
}[] {
  return quota?.purchasedCredits != null || quota?.grantedCredits != null
    ? [
        {
          label: 'agent.minimaxPurchasedCredits',
          remaining: quota.purchasedCredits,
          total: quota.purchasedTotal,
          expiresAt: quota.purchasedExpiresAt,
        },
        {
          label: 'agent.grantedCredits',
          remaining: quota.grantedCredits,
          total: quota.grantedTotal,
          expiresAt: quota.grantedExpiresAt,
        },
      ]
    : [];
}

export function minimaxRewardStatus(
  account: Pick<MinimaxAccount, 'dailyReward'>,
  now = Date.now() / 1000
): 'unknown' | 'claimable' | 'claimed' | 'unavailable' {
  const reward = account.dailyReward;
  return reward &&
    Number.isFinite(reward.checkedAt) &&
    reward.checkedAt > 0 &&
    reward.checkedAt + MINIMAX_REWARD_CACHE_SECONDS > now
    ? reward.status
    : 'unknown';
}
