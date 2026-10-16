import { forwardRef } from 'react';

interface CareerTooltipProps {
  text: string;
  left: number;
  top: number;
  fixed?: boolean;
}

export const CareerTooltip = forwardRef<HTMLDivElement, CareerTooltipProps>(function CareerTooltip(
  { text, left, top, fixed = false },
  ref
) {
  return (
    <div
      ref={ref}
      role="tooltip"
      className={`pointer-events-none ${fixed ? 'fixed' : 'absolute'} z-20 -translate-x-1/2 -translate-y-full rounded-md border border-cyber-border bg-cyber-elevated px-2 py-1 text-xs text-cyber-text whitespace-nowrap shadow-lg`}
      style={{ left, top }}
    >
      {text}
    </div>
  );
});
