import type { ProxyAccount } from '../../api/accountProxy';
import type { QoderEdition } from '../../api/qoder';
import type { TKey } from '../../i18n';
import { codexPlanLabel } from './codexPlanLabel';

export function isFreePlan(plan?: string | null): boolean {
  return plan?.trim().toLowerCase() === 'free';
}

export function accountPlanLabel(plan: string): string {
  return isFreePlan(plan) ? 'Free' : plan.replace(/^[a-z]/, (letter) => letter.toUpperCase());
}

export function minimaxPlanLabel(plan?: string | null): string {
  return plan?.replace(/^M Plan (Free|Go|Explore|Build)$/, '$1') ?? '';
}

export function qoderPlanLabel(
  plan?: string | null,
  edition: QoderEdition = 'qoder',
  t?: (key: TKey) => string
): string {
  if (!plan) return '';
  const key = plan.trim().toLowerCase();
  if (['free', 'personal_standard', 'personal_professional_trial'].includes(key))
    return edition === 'qodercn' ? (t?.('agent.qoderTrial') ?? 'Trial') : 'Free';
  const labels: Record<string, string> = {
    pro: 'Pro',
    'pro+': 'Pro+',
    ultra: 'Ultra',
    personal_professional: 'Pro',
    personal_professional_plus: 'Pro+',
    teams: 'Teams',
    enterprise: 'Enterprise',
    enterprise_standard: 'Enterprise Standard',
    enterprise_professional: 'Enterprise VPC',
  };
  return Object.prototype.hasOwnProperty.call(labels, key) ? labels[key] : accountPlanLabel(plan);
}

export function accountProxyPlanLabel(account: ProxyAccount, t?: (key: TKey) => string): string {
  if (account.source.provider === 'minimax') return minimaxPlanLabel(account.plan);
  if (account.source.provider === 'qoder' || account.source.provider === 'qoderCn')
    return qoderPlanLabel(
      account.plan,
      account.source.provider === 'qoderCn' ? 'qodercn' : 'qoder',
      t
    );
  return account.source.provider === 'codex'
    ? codexPlanLabel(account.plan)
    : account.plan
      ? accountPlanLabel(account.plan)
      : '';
}

export function accountProxyIdentityLabel(account: ProxyAccount, t: (key: TKey) => string): string {
  return account.source.provider === 'minimax' && account.region
    ? `${t(account.region === 'cn' ? 'agent.minimaxRegionCn' : 'agent.minimaxRegionGlobal')} · ${account.label}`
    : account.label;
}
