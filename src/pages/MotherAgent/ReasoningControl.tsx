import { useEffect, useRef, useState, type CSSProperties } from 'react';
import type { ReasoningEffort } from '../../api/types';
import { useI18n } from '../../hooks/useI18n';

interface Props {
  levels: ReasoningEffort[];
  value?: ReasoningEffort;
  disabled: boolean;
  onChange: (effort: ReasoningEffort | undefined) => void;
}

export function ReasoningControl({ levels, value, disabled, onChange }: Props) {
  const { t } = useI18n();
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const choices = [undefined, ...levels];
  const index = Math.max(0, choices.indexOf(value));
  const label = t(value ? `mother.reasoning.${value}` : 'mother.reasoning.default');
  const unavailable = disabled || !levels.length;

  useEffect(() => {
    if (!isOpen) return;
    const outside = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) setIsOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setIsOpen(false);
        triggerRef.current?.focus();
      }
    };
    document.addEventListener('mousedown', outside);
    document.addEventListener('keydown', escape);
    return () => {
      document.removeEventListener('mousedown', outside);
      document.removeEventListener('keydown', escape);
    };
  }, [isOpen]);

  return (
    <div ref={containerRef} className="relative shrink-0">
      <button
        ref={triggerRef}
        type="button"
        disabled={unavailable}
        aria-label={`${t('mother.reasoning.title')}: ${label}`}
        aria-expanded={isOpen && !unavailable}
        aria-haspopup="dialog"
        onClick={() => !unavailable && setIsOpen((open) => !open)}
        className={`inline-grid whitespace-nowrap px-2 py-1 text-xs font-mono rounded-button border border-cyber-border transition-all disabled:opacity-50 ${value === 'max' ? 'text-cyber-accent hover:bg-cyber-accent/10 hover:brightness-110' : 'text-cyber-text-secondary hover:bg-cyber-text/10 hover:text-cyber-text'}`}
      >
        {choices.map((effort) => (
          <span
            key={effort || 'default'}
            aria-hidden="true"
            className="invisible col-start-1 row-start-1"
          >
            {t(effort ? `mother.reasoning.${effort}` : 'mother.reasoning.default')}
          </span>
        ))}
        <span className="col-start-1 row-start-1">{label}</span>
      </button>
      {isOpen && !unavailable && (
        <div
          role="dialog"
          aria-label={t('mother.reasoning.title')}
          className="absolute bottom-full mb-2 right-0 z-50 w-[202px] rounded-lg border border-cyber-border bg-cyber-surface p-2 shadow-xl"
        >
          <div className="mb-1 flex items-center justify-between gap-3 text-xs">
            <span className="text-cyber-text-secondary">{t('mother.reasoning.title')}</span>
            <span className={value === 'max' ? 'text-cyber-accent' : 'text-cyber-text'}>
              {label}
            </span>
          </div>
          <input
            type="range"
            min={0}
            max={levels.length}
            step={1}
            value={index}
            autoFocus
            aria-label={t('mother.reasoning.title')}
            aria-valuetext={label}
            onChange={(event) => {
              if (!unavailable) onChange(choices[Number(event.currentTarget.value)]);
            }}
            className="mother-reasoning-slider block m-0 w-full"
            style={
              {
                '--reasoning-progress': `calc(16px + (100% - 32px) * ${index / levels.length})`,
              } as CSSProperties
            }
          />
        </div>
      )}
    </div>
  );
}
