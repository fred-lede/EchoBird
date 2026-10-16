import React, { useEffect, useState } from 'react';

function formatQuotaCountdown(resetAt: number, now: number, compact?: boolean): string {
  const minutes = Math.max(0, Math.ceil((resetAt * 1000 - now) / 60_000));
  if (minutes >= (compact ? 48 : 24) * 60) {
    const days = Math.floor(minutes / (24 * 60));
    const hours = Math.floor((minutes % (24 * 60)) / 60);
    return `${days}d${hours}h`;
  }
  if (minutes >= 60) {
    return `${Math.floor(minutes / 60)}h${minutes % 60}m`;
  }
  return `${minutes}m`;
}

export const QuotaCountdown: React.FC<{
  resetAt?: number | null;
  compact?: boolean;
  small?: boolean;
  parenthesized?: boolean;
  label?: string;
  keepZero?: boolean;
}> = ({ resetAt, compact, small, parenthesized, label, keepZero }) => {
  const [now, setNow] = useState<number | null>(null);
  useEffect(() => {
    if (!resetAt) return;
    const initial = setTimeout(() => setNow(Date.now()), 0);
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    return () => {
      clearTimeout(initial);
      clearInterval(timer);
    };
  }, [resetAt]);
  if (compact && (!resetAt || now === null || (!keepZero && resetAt * 1000 <= now))) return null;
  const countdown = resetAt && now !== null ? formatQuotaCountdown(resetAt, now, compact) : '';
  return (
    <span
      aria-label={label && countdown ? `${label} ${countdown}` : undefined}
      className={`${compact ? '' : 'w-[64px] '}inline-flex flex-shrink-0 items-center justify-center align-middle text-center ${small ? 'text-[10px]' : 'text-[12px]'} font-semibold leading-[16px] text-cyber-text`}
    >
      {parenthesized && countdown ? `(${countdown})` : countdown}
    </span>
  );
};
