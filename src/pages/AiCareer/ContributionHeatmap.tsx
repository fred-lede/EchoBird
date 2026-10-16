// Contribution heatmap grid. Fills the full row width (responsive square cells
// via CSS grid) so it lines up with the stat row and family cards above/below
// it. Colours use the theme's coral accent (`--accent-rgb`) via CSS vars.
// Hovering a cell shows a floating tooltip, horizontally clamped so it never
// spills past the grid edge (which an ancestor's overflow would clip).
//
// Intro: during enter each cell lights to a RANDOM coral shade (same hue),
// staggered right→left, so a wave of VARIED colour visibly propagates across
// the grid (a uniform colour would just look like a solid block appearing).
// Then each cell hands over to its REAL colour, right→left (exit), and the
// heatmap sits at rest (done). Real usage is hidden until the exit.
//
// The intro is driven entirely in JS (requestAnimationFrame writing each cell's
// background-color). We deliberately do NOT use CSS @keyframes: a keyframe that
// animates `background-color` to a `var()` value silently fails to render on
// some engines — WebKitGTK (Linux), older WebView2, and Windows Remote-Desktop
// sessions — so the reveal just wouldn't play there. Plain per-frame style
// writes are universal.

import { useLayoutEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { useI18n } from '../../hooks/useI18n';
import { useNavigationStore } from '../../stores/navigationStore';
import { buildGrid, DAYS, WEEKS, localDayKey, type DayBuckets } from './heatmapData';
import { CareerTooltip } from './CareerTooltip';

// Rest colours (theme-tracking var strings): empty day is a faint wash of the
// primary text colour; levels 1–4 ramp the coral accent's opacity.
const LEVEL_BG = [
  'rgb(var(--text-primary-rgb) / 0.06)',
  'rgb(var(--accent-rgb) / 0.32)',
  'rgb(var(--accent-rgb) / 0.52)',
  'rgb(var(--accent-rgb) / 0.74)',
  'rgb(var(--accent-rgb) / 0.96)',
];
// The matching opacities, used to build resolved rgba for the JS-driven intro.
const LEVEL_ALPHA = [0.06, 0.32, 0.52, 0.74, 0.96];
const EMPTY_ALPHA = 0.06;

// Loading coral shown during enter (no real data): each cell gets a RANDOM
// coral brightness in [LOAD_MIN, LOAD_MAX] (same hue) so the staggered
// propagation is actually visible — a uniform colour looks like a solid block.
const LOAD_MIN = 0.28;
const LOAD_MAX = 0.82;

// Fallback RGB triples if a theme var can't be read (shouldn't happen).
const ACCENT_FALLBACK: RGB = [217, 119, 87];
const TEXT_FALLBACK: RGB = [230, 228, 224];

// Deterministic per-cell pseudo-random in [0, 1) (stable across renders).
function cellRand(n: number): number {
  const s = Math.sin(n * 12.9898 + 78.233) * 43758.5453;
  return s - Math.floor(s);
}

// Intro timing. ENTER_STEP/JITTER spread the right→left propagation; CELL_FADE
// is the per-cell colour fade. SWEEP_MS = how long the whole staggered sweep
// takes (last cell's delay + its fade), timing the enter → exit → done handoff.
const ENTER_STEP = 0.03; // seconds per column
const ENTER_JITTER = 0.3; // seconds, total spread of the random edge scatter
const CELL_FADE = 0.45; // seconds, the per-cell colour fade
const SWEEP_MS = Math.round(((WEEKS - 1) * ENTER_STEP + ENTER_JITTER / 2 + CELL_FADE) * 1000);

const GRID_STYLE: CSSProperties = {
  display: 'grid',
  gridTemplateColumns: `repeat(${WEEKS}, minmax(0, 1fr))`,
  gridTemplateRows: `repeat(${DAYS}, minmax(0, 1fr))`,
  gridAutoFlow: 'column',
  gap: 4,
  width: '100%',
  aspectRatio: `${WEEKS} / ${DAYS}`,
};

type RGB = [number, number, number];
type RGBA = [number, number, number, number];

const clamp01 = (x: number): number => (x < 0 ? 0 : x > 1 ? 1 : x);
// Ease-out (quadratic) so the per-cell fade decelerates, matching the feel of
// the previous CSS `ease-out` keyframe.
const easeOut = (p: number): number => 1 - (1 - p) * (1 - p);

// Parse a `"R G B"` (space- or comma-separated) custom property into a triple.
function parseRgbVar(root: Element, name: string, fallback: RGB): RGB {
  const raw = getComputedStyle(root).getPropertyValue(name).trim();
  const parts = raw
    .split(/[\s,]+/)
    .map(Number)
    .filter((n) => !Number.isNaN(n));
  return parts.length >= 3 ? [parts[0], parts[1], parts[2]] : fallback;
}

const rgbaStr = (c: RGBA): string =>
  `rgba(${Math.round(c[0])}, ${Math.round(c[1])}, ${Math.round(c[2])}, ${c[3].toFixed(3)})`;

function lerpRgba(a: RGBA, b: RGBA, t: number): string {
  return rgbaStr([
    a[0] + (b[0] - a[0]) * t,
    a[1] + (b[1] - a[1]) * t,
    a[2] + (b[2] - a[2]) * t,
    a[3] + (b[3] - a[3]) * t,
  ]);
}

interface CellIntro {
  future: boolean;
  level: number;
  delayMs: number;
  loadAlpha: number;
}

interface TipState {
  text: string;
  cx: number; // desired centre x, relative to the grid root
  top: number; // y, relative to the grid root
}

export function ContributionHeatmap({
  buckets,
  refreshKey,
}: {
  buckets: DayBuckets;
  refreshKey: number;
}) {
  const { t, locale } = useI18n();
  const today = localDayKey(new Date());
  const grid = useMemo(() => buildGrid(buckets, new Date(`${today}T00:00:00`)), [buckets, today]);
  // Column-major flatten: grid[col][row] → fills the CSS grid top-to-bottom
  // then left-to-right (gridAutoFlow: column).
  const cells = useMemo(() => grid.flat(), [grid]);

  const rootRef = useRef<HTMLDivElement>(null);
  const tipRef = useRef<HTMLDivElement>(null);
  const cellRefs = useRef<(HTMLDivElement | null)[]>([]);
  const [tip, setTip] = useState<TipState | null>(null);
  const [tipLeft, setTipLeft] = useState(0);
  const activePage = useNavigationStore((s) => s.activePage);
  const hasSessions = buckets.sessions.size > 0;

  // Per-cell intro metadata: right→left stagger delay + random loading
  // brightness. Stable for a given grid (recomputed only when buckets change).
  const intro = useMemo<CellIntro[]>(
    () =>
      cells.map((cell, i) => {
        const col = Math.floor(i / DAYS);
        // Hashed jitter → slightly scattered propagation edge.
        const jitter = (((i * 0.6180339887) % 1) - 0.5) * ENTER_JITTER;
        const delayMs = Math.max(0, (WEEKS - 1 - col) * ENTER_STEP + jitter) * 1000;
        const loadAlpha = LOAD_MIN + (LOAD_MAX - LOAD_MIN) * cellRand(i);
        return { future: cell.future, level: cell.level, delayMs, loadAlpha };
      }),
    [cells]
  );
  const introRef = useRef(intro);
  const repaintRef = useRef<(() => void) | null>(null);

  useLayoutEffect(() => {
    introRef.current = intro;
    // React may apply a new rest colour; keep the current reveal before paint.
    if (repaintRef.current) repaintRef.current();
    else
      intro.forEach((cell, i) => {
        const el = cellRefs.current[i];
        if (el) el.style.backgroundColor = cell.future ? 'transparent' : LEVEL_BG[cell.level];
      });
  }, [intro]);

  // The intro: enter (propagate loading coral) → exit (reveal real) → done.
  // Replay on entry or manual refresh. Background data updates the reveal
  // target without restarting the animation. Commit the grey start before paint.
  useLayoutEffect(() => {
    if (activePage !== 'aiCareer' || !hasSessions) return;
    const root = rootRef.current;
    if (!root) return;
    const els = cellRefs.current;

    const accent = parseRgbVar(root, '--accent-rgb', ACCENT_FALLBACK);
    const text = parseRgbVar(root, '--text-primary-rgb', TEXT_FALLBACK);
    const grey: RGBA = [text[0], text[1], text[2], EMPTY_ALPHA];
    const loadOf = (a: number): RGBA => [accent[0], accent[1], accent[2], a];
    const realOf = (lvl: number): RGBA =>
      lvl === 0 ? grey : [accent[0], accent[1], accent[2], LEVEL_ALPHA[lvl]];
    const FADE = CELL_FADE * 1000;

    let raf = 0;
    let start = 0;
    let elapsed = 0;
    const paint = () => {
      const intro = introRef.current;
      for (let i = 0; i < intro.length; i++) {
        const el = els[i];
        const m = intro[i];
        if (!el || m.future) continue;
        if (elapsed < SWEEP_MS) {
          // enter: faint grey → this cell's random loading coral
          const p = easeOut(clamp01((elapsed - m.delayMs) / FADE));
          el.style.backgroundColor = lerpRgba(grey, loadOf(m.loadAlpha), p);
        } else if (elapsed < SWEEP_MS * 2) {
          // exit: loading coral → the cell's real colour (usage revealed now)
          const p = easeOut(clamp01((elapsed - SWEEP_MS - m.delayMs) / FADE));
          el.style.backgroundColor = lerpRgba(loadOf(m.loadAlpha), realOf(m.level), p);
        } else {
          // Rest colours track theme changes after the reveal.
          el.style.backgroundColor = LEVEL_BG[m.level];
        }
      }
    };
    repaintRef.current = paint;
    paint();
    const frame = (ts: number) => {
      if (!start) start = ts;
      elapsed = ts - start;
      paint();
      if (elapsed >= SWEEP_MS * 2) return;
      raf = window.requestAnimationFrame(frame);
    };
    raf = window.requestAnimationFrame(frame);

    return () => {
      window.cancelAnimationFrame(raf);
      repaintRef.current = null;
    };
  }, [activePage, hasSessions, refreshKey]);

  // Clamp the tooltip horizontally so its full box stays inside the grid
  // width. Runs before paint, so there's no flash at the unclamped spot.
  useLayoutEffect(() => {
    if (!tip || !tipRef.current || !rootRef.current) return;
    const half = tipRef.current.offsetWidth / 2;
    const rootW = rootRef.current.offsetWidth;
    setTipLeft(Math.max(half + 2, Math.min(tip.cx, rootW - half - 2)));
  }, [tip]);

  const tipText = (date: string, count: number): string => {
    const d = new Date(`${date}T00:00:00`);
    const dateLabel = d.toLocaleDateString(locale, { month: 'short', day: 'numeric' });
    const countLabel =
      count > 0
        ? t('aiCareer.messages').replace('{count}', String(count))
        : t('aiCareer.noActivity');
    return `${dateLabel} · ${countLabel}`;
  };

  return (
    <div ref={rootRef} className="relative w-full" onMouseLeave={() => setTip(null)}>
      <div style={GRID_STYLE}>
        {cells.map((cell, i) => (
          <div
            key={cell.date}
            ref={(el) => {
              cellRefs.current[i] = el;
            }}
            onMouseEnter={(e) => {
              if (cell.future) {
                setTip(null);
                return;
              }
              const root = rootRef.current;
              if (!root) return;
              const rb = root.getBoundingClientRect();
              const cb = e.currentTarget.getBoundingClientRect();
              setTip({
                text: tipText(cell.date, cell.count),
                cx: cb.left - rb.left + cb.width / 2,
                top: cb.top - rb.top - 4,
              });
            }}
            style={{
              borderRadius: 3,
              // React renders the cell's REST colour; the rAF intro overrides
              // it per frame and settles back here. Constant across renders, so
              // React never re-applies it (and never fights the rAF writes).
              background: cell.future ? 'transparent' : LEVEL_BG[cell.level],
              cursor: 'default',
            }}
          />
        ))}
      </div>

      <div className="mt-2.5 flex items-center justify-end gap-1.5 text-xs text-cyber-text-secondary">
        <span>{t('aiCareer.legendLess')}</span>
        {LEVEL_BG.map((bg, lv) => (
          <span
            key={lv}
            style={{
              width: 12,
              height: 12,
              borderRadius: 3,
              background: bg,
              display: 'inline-block',
            }}
          />
        ))}
        <span>{t('aiCareer.legendMore')}</span>
      </div>

      {tip && <CareerTooltip ref={tipRef} text={tip.text} left={tipLeft} top={tip.top} />}
    </div>
  );
}
