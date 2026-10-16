import { expect, it } from 'vitest';
import { en } from '../../i18n/en';
import zhHans from '../../i18n/zh-Hans';
import zhHant from '../../i18n/zh-Hant';
import ja from '../../i18n/ja';
import { accountProxyPlanLabel, qoderPlanLabel } from './accountPlanLabel';
import {
  qoderExpiry,
  qoderQuotaLabel,
  qoderRewardStatus,
  qoderSubscriptionExpiry,
} from './qoderAccountDisplay';

it('preserves confirmed reward status until UTC+8 10:00 then requires an explicit recheck', () => {
  const now = Date.parse('2026-10-13T01:59:30Z') / 1000;
  const resetAt = Date.parse('2026-10-13T02:00:00Z') / 1000;
  for (const status of ['claimable', 'claimed', 'unavailable'] as const) {
    const account = { dailyReward: { status, checkedAt: now - 30, resetAt } };
    expect(qoderRewardStatus(account, now)).toBe(status);
    expect(qoderRewardStatus(account, resetAt)).toBe('unknown');
  }
  expect(qoderRewardStatus({}, now)).toBe('unknown');
  expect(qoderRewardStatus({ dailyReward: null }, now)).toBe('unknown');
  expect(
    qoderRewardStatus({ dailyReward: { status: 'claimed', checkedAt: now + 1, resetAt } }, now)
  ).toBe('claimed');
  expect(
    qoderRewardStatus({ dailyReward: { status: 'claimed', checkedAt: resetAt, resetAt } }, now)
  ).toBe('unknown');
  expect(
    qoderRewardStatus({ dailyReward: { status: 'claimable', checkedAt: now, resetAt: NaN } }, now)
  ).toBe('unknown');
});

it.each([
  ['personal_standard', 'Free'],
  ['personal_professional_trial', 'Free'],
  ['personal_professional', 'Pro'],
  ['personal_professional_plus', 'Pro+'],
  ['teams', 'Teams'],
  ['enterprise', 'Enterprise'],
  ['enterprise_standard', 'Enterprise Standard'],
  ['enterprise_professional', 'Enterprise VPC'],
])('uses the official short plan for %s in the global pool', (plan, label) => {
  expect(qoderPlanLabel(plan)).toBe(label);
  expect(
    accountProxyPlanLabel({
      source: { provider: 'qoder', accountId: 'saved' },
      label: 'account',
      plan,
    })
  ).toBe(label);
});

it.each([en, zhHans, zhHant, ja])(
  'localizes CN trial and keeps the four personal tiers in both pools',
  (translations) => {
    const t = (key: keyof typeof en) => translations[key] ?? en[key];
    for (const plan of ['free', 'personal_standard', 'personal_professional_trial']) {
      expect(qoderPlanLabel(plan, 'qodercn', t)).toBe(t('agent.qoderTrial'));
      expect(
        accountProxyPlanLabel(
          { source: { provider: 'qoderCn', accountId: 'saved' }, label: 'account', plan },
          t
        )
      ).toBe(t('agent.qoderTrial'));
      expect(qoderPlanLabel(plan, 'qoder', t)).toBe('Free');
    }
    for (const plan of ['Pro', 'Pro+', 'Ultra']) {
      for (const provider of ['qoder', 'qoderCn'] as const) {
        expect(
          accountProxyPlanLabel(
            { source: { provider, accountId: 'saved' }, label: 'account', plan },
            t
          )
        ).toBe(plan);
      }
    }
  }
);

it('keeps unknown plans distinct and does not alter the other providers', () => {
  expect(qoderPlanLabel('future_tier')).toBe('Future_tier');
  expect(qoderPlanLabel('constructor')).toBe('Constructor');
  expect(qoderPlanLabel(null)).toBe('');
  expect(
    accountProxyPlanLabel({
      source: { provider: 'workBuddy', accountId: 'saved' },
      label: 'account',
      plan: 'personal_standard',
    })
  ).toBe('Personal_standard');
});

it.each([en, zhHans, zhHant, ja])(
  'localizes known buckets while preserving named resource packs',
  (translations) => {
    const t = (key: keyof typeof en) => translations[key] ?? en[key];
    expect(qoderQuotaLabel('Credits', t)).toBe(t('agent.baseCredits'));
    expect(qoderQuotaLabel('Add-on', t)).toBe(t('agent.additionalCredits'));
    expect(qoderQuotaLabel('Organization', t)).toBe(t('agent.organizationCredits'));
    expect(qoderQuotaLabel('Dedicated', t)).toBe(t('agent.personalResourceCredits'));
    expect(qoderQuotaLabel('Project allocation', t)).toBe('Project allocation');
    expect(qoderQuotaLabel('constructor', t)).toBe('constructor');
  }
);

it('hides cached permanent and expired dates and retains valid seconds or milliseconds', () => {
  const now = 1_790_000_000;
  for (const value of [
    null,
    undefined,
    0,
    -1,
    now,
    now - 1,
    NaN,
    Infinity,
    253_402_214_400,
    253_402_214_400_000,
    Date.parse('9999-01-01T00:00:00Z') / 1000,
    Date.parse('9999-12-30T00:00:00Z') / 1000,
    Date.parse('9999-12-31T00:00:00+08:00') / 1000,
    Date.parse('9999-12-31T00:00:00+08:00'),
  ]) {
    expect(qoderExpiry(value, now)).toBeUndefined();
  }
  expect(qoderExpiry(now + 86400, now)).toBe(now + 86400);
  expect(qoderExpiry((now + 86400) * 1000, now)).toBe(now + 86400);
  expect(qoderExpiry(Date.parse('9998-12-31T23:59:59Z') / 1000, now)).toBe(
    Date.parse('9998-12-31T23:59:59Z') / 1000
  );
});

it('hides standard account expiry and retains a normal trial or unknown plan date', () => {
  const future = Date.now() / 1000 + 86400;
  expect(qoderSubscriptionExpiry({ plan: 'personal_standard', expiresAt: future })).toBeUndefined();
  for (const plan of [
    'personal_professional',
    'personal_professional_trial',
    'future_tier',
    null,
  ]) {
    expect(qoderSubscriptionExpiry({ plan, expiresAt: future })).toBe(future);
  }
});
