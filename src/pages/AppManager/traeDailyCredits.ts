import { useEffect, useState } from 'react';
import type { TraeAccount } from '../../api/tauri';

export const traeClaimedToday = (at?: number | null, now = Date.now()) =>
  at != null && Math.floor((at + 8 * 3600) / 86400) === Math.floor((now / 1000 + 8 * 3600) / 86400);

export const traeClaimUnavailable = (
  account: TraeAccount,
  accounts: TraeAccount[],
  now = Date.now()
) =>
  account.edition !== 'traecn' ||
  account.rewardEnabled === false ||
  traeClaimedToday(account.dailyClaimedAt, now) ||
  accounts.some((row) => row.edition === 'traecn' && traeClaimedToday(row.deviceClaimedAt, now));

export function useTraeClaimedToday(enabled: boolean) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (!enabled) return;
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(timer);
  }, [enabled]);
  return (at?: number | null) => traeClaimedToday(at, now);
}
