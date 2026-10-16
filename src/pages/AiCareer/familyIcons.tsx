import { useState } from 'react';
import type { AiCareerFamily } from '../../api/aiCareer';

export const CAREER_FAMILIES: ReadonlyArray<{
  id: AiCareerFamily;
  name: string;
  toolId: string;
  format: 'svg' | 'png';
}> = [
  { id: 'claude', name: 'Claude', toolId: 'claudecode', format: 'svg' },
  { id: 'codex', name: 'Codex', toolId: 'codex', format: 'svg' },
  { id: 'opencode', name: 'OpenCode', toolId: 'opencode', format: 'svg' },
  { id: 'deepseek', name: 'DeepSeek', toolId: 'dsh', format: 'svg' },
  { id: 'hermes', name: 'Hermes', toolId: 'hermes', format: 'png' },
  { id: 'mimo', name: 'MiMo', toolId: 'mimocode', format: 'png' },
];

export function FamilyIcon({
  family,
  className = 'w-8 h-8',
}: {
  family: AiCareerFamily;
  className?: string;
}) {
  const icon = CAREER_FAMILIES.find((entry) => entry.id === family)!;
  const source = './icons/tools/' + icon.toolId + '.' + icon.format;
  const [fallbackFor, setFallbackFor] = useState<string | null>(null);
  const src = fallbackFor === source ? './icons/tools/' + icon.toolId + '.png' : source;
  return (
    <img
      src={src}
      alt={icon.name}
      className={className + ' flex-shrink-0 object-contain'}
      onError={() => {
        if (src.endsWith('.svg')) setFallbackFor(source);
      }}
    />
  );
}
