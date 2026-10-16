import { expect, it } from 'vitest';
import { accountProxyPlanLabel, minimaxPlanLabel } from './accountPlanLabel';
import { minimaxRewardStatus } from './minimaxAccountDisplay';

it.each(['Free', 'Go', 'Explore', 'Build'])(
  'removes only the known MiniMax %s tier prefix in the pool',
  (tier) => {
    const plan = `M Plan ${tier}`;
    expect(minimaxPlanLabel(plan)).toBe(tier);
    expect(
      accountProxyPlanLabel({
        source: { provider: 'minimax', accountId: 'saved' },
        label: 'saved',
        plan,
      })
    ).toBe(tier);
  }
);

it('keeps unknown MiniMax plans intact and does not strip another provider plan', () => {
  for (const plan of [
    'M Plan Pro',
    'M Plan Future',
    'M Plan Explore Plus',
    'Free',
    'custom plan',
  ]) {
    expect(minimaxPlanLabel(plan)).toBe(plan);
    expect(
      accountProxyPlanLabel({
        source: { provider: 'minimax', accountId: 'saved' },
        label: 'saved',
        plan,
      })
    ).toBe(plan);
  }
  expect(minimaxPlanLabel(null)).toBe('');
  expect(minimaxPlanLabel(undefined)).toBe('');
  expect(
    accountProxyPlanLabel({
      source: { provider: 'workBuddy', accountId: 'saved' },
      label: 'saved',
      plan: 'M Plan Go',
    })
  ).toBe('M Plan Go');
});

it('expires confirmed MiniMax reward status after five minutes without inventing a daily reset', () => {
  const now = 1_790_000_000;
  for (const status of ['claimable', 'claimed', 'unavailable'] as const) {
    const account = { dailyReward: { status, checkedAt: now } };
    expect(minimaxRewardStatus(account, now + 299)).toBe(status);
    expect(minimaxRewardStatus(account, now + 300)).toBe('unknown');
  }
  expect(minimaxRewardStatus({}, now)).toBe('unknown');
  expect(minimaxRewardStatus({ dailyReward: null }, now)).toBe('unknown');
  expect(minimaxRewardStatus({ dailyReward: { status: 'claimed', checkedAt: NaN } }, now)).toBe(
    'unknown'
  );
});
