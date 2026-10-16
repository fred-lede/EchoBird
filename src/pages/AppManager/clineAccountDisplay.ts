export function clineBalanceLabel(balanceMicros: number | null | undefined, locale: string) {
  return balanceMicros == null
    ? '—'
    : new Intl.NumberFormat(locale, {
        style: 'currency',
        currency: 'USD',
        maximumFractionDigits: 2,
      }).format(balanceMicros / 1_000_000);
}
