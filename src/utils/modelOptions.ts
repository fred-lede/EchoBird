export function isFreeModelOption(
  id: string,
  metadata: { free?: boolean; multiplier?: number } = {},
  inferFreeFromId = true
): boolean {
  if (typeof metadata.free === 'boolean') return metadata.free;
  const { multiplier } = metadata;
  if (typeof multiplier === 'number' && Number.isFinite(multiplier) && multiplier >= 0)
    return multiplier === 0;
  return inferFreeFromId && id.toLowerCase().includes('free');
}
