import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, Copy, FolderSearch, Hash, Terminal, type LucideIcon } from 'lucide-react';
import { useI18n } from '../../hooks/useI18n';
import { useAiCareerStore } from '../../stores/aiCareerStore';
import { useToast } from '../../components/Toast';
import { copyText } from '../../utils/copyText';
import {
  aiCareerFamilyHistory,
  revealAiCareerSessionFile,
  type AiCareerFamily,
  type SavedSession,
} from '../../api/aiCareer';
import { CareerTooltip } from './CareerTooltip';
import { formatCompact } from './heatmapData';
import { sessionProject, sessionResumeCommand } from './sessionActions';
import { FamilyIcon } from './familyIcons';

const PAGE = 20;
const SKELETON_WIDTHS = ['72%', '58%', '80%', '52%', '68%', '76%', '61%'];

interface Row extends SavedSession {
  savedMs: number;
  nowMs: number;
}

function toRows(list: SavedSession[]): Row[] {
  const nowMs = Date.now();
  return list.map((session) => {
    const parsed = Date.parse(session.saved_at);
    const numeric = Number(session.saved_at);
    const savedMs = !Number.isNaN(parsed)
      ? parsed
      : numeric > 0
        ? numeric < 1e11
          ? numeric * 1000
          : numeric
        : nowMs;
    return { ...session, savedMs, nowMs };
  });
}

function relativeTime(row: Row, locale: string, t: ReturnType<typeof useI18n>['t']): string {
  const date = new Date(row.savedMs);
  const diff = row.nowMs - row.savedMs;
  if (diff < 3_600_000) return t('aiCareer.timeJustNow');
  if (new Date(row.nowMs).toDateString() === date.toDateString()) return t('aiCareer.timeToday');
  if (new Date(row.nowMs - 86_400_000).toDateString() === date.toDateString())
    return t('aiCareer.timeYesterday');
  const days = Math.floor(diff / 86_400_000);
  if (days < 7) return t('aiCareer.timeDaysAgo').replace('{days}', String(days));
  return date.toLocaleDateString(locale, { month: 'short', day: 'numeric' });
}

function SkeletonRow({ w }: { w: string }) {
  return (
    <div className="rounded-lg border border-cyber-border/40 bg-cyber-text/[0.02] px-3 py-3 space-y-2">
      <div className="h-3 rounded bg-cyber-text/10 animate-pulse" style={{ width: w }} />
      <div className="h-2.5 w-2/5 rounded bg-cyber-text/10 animate-pulse" />
    </div>
  );
}

function SessionActionButton({
  label,
  Icon,
  disabled,
  copied,
  onClick,
  onTip,
  clearTip,
}: {
  label: string;
  Icon: LucideIcon;
  disabled: boolean;
  copied: boolean;
  onClick: () => void;
  onTip: (target: HTMLButtonElement, label: string) => void;
  clearTip: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
      onMouseEnter={(event) => onTip(event.currentTarget, label)}
      onMouseLeave={clearTip}
      onFocus={(event) => onTip(event.currentTarget, label)}
      onBlur={clearTip}
      className="p-1.5 rounded-md text-cyber-text-muted hover:text-cyber-text hover:bg-cyber-text/10 disabled:opacity-30 disabled:hover:bg-transparent disabled:hover:text-cyber-text-muted transition-colors"
    >
      {copied ? <Check size={14} className="text-cyber-accent" /> : <Icon size={14} />}
    </button>
  );
}

export function AiCareerPanel({ family }: { family: AiCareerFamily }) {
  const refreshKey = useAiCareerStore((s) => s.refreshKey);
  return <FamilySessions key={`${family}::${refreshKey}`} family={family} />;
}

function FamilySessions({ family }: { family: AiCareerFamily }) {
  const { t, locale } = useI18n();
  const { showToast } = useToast();
  const [initial] = useState(() => {
    const state = useAiCareerStore.getState();
    return { sessions: state.cachedScopes[family]?.sessions, explicit: state.refreshing };
  });
  const [rows, setRows] = useState<Row[]>(() => toRows(initial.sessions ?? []));
  const [loading, setLoading] = useState(true);
  const [firstPageReady, setFirstPageReady] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [failed, setFailed] = useState(false);
  const [copiedAction, setCopiedAction] = useState<string | null>(null);
  const [tip, setTip] = useState<{ text: string; left: number; top: number } | null>(null);
  const [tipLeft, setTipLeft] = useState(0);
  const activeRef = useRef(false);
  const pendingRef = useRef(false);
  const offsetRef = useRef(0);
  const copyTimerRef = useRef<number>();
  const sentinelRef = useRef<HTMLDivElement>(null);
  const tipRef = useRef<HTMLDivElement>(null);

  const loadPage = useCallback(
    async (explicit = false) => {
      if (!activeRef.current || pendingRef.current) return;
      pendingRef.current = true;
      const offset = offsetRef.current;
      setLoading(true);
      setFailed(false);
      try {
        const list = await aiCareerFamilyHistory(family, offset, PAGE);
        if (!activeRef.current) return;
        if (offset === 0) {
          setRows(toRows(list));
          setFirstPageReady(true);
          useAiCareerStore.getState().cacheSessions(family, list);
        } else {
          setRows((previous) => [...previous, ...toRows(list)]);
        }
        offsetRef.current = offset + list.length;
        setHasMore(list.length === PAGE);
      } catch {
        if (
          activeRef.current &&
          (offset > 0 || initial.sessions === undefined || initial.explicit || explicit)
        )
          setFailed(true);
      } finally {
        pendingRef.current = false;
        if (activeRef.current) setLoading(false);
      }
    },
    [family, initial]
  );

  useEffect(() => {
    activeRef.current = true;
    // The keyed instance owns this family/refresh request generation.
    void Promise.resolve().then(() => {
      if (activeRef.current) void loadPage();
    });
    return () => {
      activeRef.current = false;
      window.clearTimeout(copyTimerRef.current);
    };
  }, [loadPage]);

  useEffect(() => {
    if (loading || failed || !hasMore) return;
    const sentinel = sentinelRef.current;
    if (!sentinel) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) void loadPage();
      },
      { rootMargin: '200px' }
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [loading, failed, hasMore, loadPage]);

  useLayoutEffect(() => {
    if (!tip || !tipRef.current) return;
    const half = tipRef.current.offsetWidth / 2;
    setTipLeft(Math.max(half + 4, Math.min(tip.left, window.innerWidth - half - 4)));
  }, [tip]);

  useEffect(() => {
    if (!tip) return;
    const clear = () => setTip(null);
    window.addEventListener('scroll', clear, true);
    window.addEventListener('resize', clear);
    return () => {
      window.removeEventListener('scroll', clear, true);
      window.removeEventListener('resize', clear);
    };
  }, [tip]);

  const showTip = (target: HTMLButtonElement, text: string) => {
    const rect = target.getBoundingClientRect();
    setTip({ text, left: rect.left + rect.width / 2, top: rect.top - 4 });
  };

  const copy = async (key: string, value: string) => {
    const success = await copyText(value);
    if (!activeRef.current) return;
    if (!success) {
      showToast('error', t('aiCareer.actionFailed'));
      return;
    }
    setCopiedAction(key);
    window.clearTimeout(copyTimerRef.current);
    copyTimerRef.current = window.setTimeout(() => setCopiedAction(null), 1500);
  };

  const reveal = async (path: string) => {
    try {
      await revealAiCareerSessionFile(path);
    } catch {
      if (activeRef.current) showToast('error', t('aiCareer.actionFailed'));
    }
  };

  return (
    <div className="w-full flex-shrink-0 space-y-2 pb-5">
      {rows.map((session) => {
        const resumeCommand = sessionResumeCommand(session.tool, session.session_token);
        const path = session.file_path;
        const input = session.input_tokens;
        const output = session.output_tokens;
        const total = input !== null && output !== null ? formatCompact(input + output) : '—';
        const metadata = [sessionProject(session.cwd), relativeTime(session, locale, t)];
        const modelId = session.model_id?.trim();
        if (modelId) metadata.push(modelId);
        if (session.message_count !== null && session.message_count !== undefined)
          metadata.push(
            t('aiCareer.messages').replace('{count}', session.message_count.toLocaleString(locale))
          );
        const rowKey = `${session.id}:${path ?? session.saved_at}`;
        return (
          <div
            key={rowKey}
            className="flex items-center gap-3 rounded-lg border border-cyber-border/40 bg-cyber-text/[0.02] px-3 py-2.5 hover:bg-cyber-text/[0.06] transition-colors"
          >
            <FamilyIcon family={family} className="w-11 h-11" />
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-3 mb-1">
                <div className="flex-1 min-w-0 text-[13px] text-cyber-text truncate">
                  {session.name}
                </div>
                <div className="flex-shrink-0 flex items-center gap-1">
                  <SessionActionButton
                    label={t('aiCareer.copyId')}
                    Icon={Hash}
                    disabled={!session.session_token}
                    copied={copiedAction === `${rowKey}:id`}
                    onClick={() => void copy(`${rowKey}:id`, session.session_token!)}
                    onTip={showTip}
                    clearTip={() => setTip(null)}
                  />
                  <SessionActionButton
                    label={t('aiCareer.copyResume')}
                    Icon={Terminal}
                    disabled={!resumeCommand}
                    copied={copiedAction === `${rowKey}:resume`}
                    onClick={() => void copy(`${rowKey}:resume`, resumeCommand!)}
                    onTip={showTip}
                    clearTip={() => setTip(null)}
                  />
                  <SessionActionButton
                    label={t('aiCareer.copyPath')}
                    Icon={Copy}
                    disabled={!path}
                    copied={copiedAction === `${rowKey}:path`}
                    onClick={() => void copy(`${rowKey}:path`, path!)}
                    onTip={showTip}
                    clearTip={() => setTip(null)}
                  />
                  <SessionActionButton
                    label={t('aiCareer.showInFolder')}
                    Icon={FolderSearch}
                    disabled={!path}
                    copied={false}
                    onClick={() => void reveal(path!)}
                    onTip={showTip}
                    clearTip={() => setTip(null)}
                  />
                </div>
              </div>
              <div className="flex items-center gap-3">
                <div className="flex-1 min-w-0 text-[11px] text-cyber-text-secondary truncate">
                  {metadata.join(' · ')}
                </div>
                <div className="flex-shrink-0 text-[11px] text-cyber-text-secondary text-right tabular-nums whitespace-nowrap">
                  {t('aiCareer.input')} {input === null ? '—' : formatCompact(input)} ·{' '}
                  {t('aiCareer.output')} {output === null ? '—' : formatCompact(output)} ·{' '}
                  {t('aiCareer.total')} {total}
                </div>
              </div>
            </div>
          </div>
        );
      })}

      {loading &&
        (firstPageReady || initial.sessions === undefined) &&
        (rows.length ? SKELETON_WIDTHS.slice(0, 3) : SKELETON_WIDTHS).map((width, i) => (
          <SkeletonRow key={`loading-${i}`} w={width} />
        ))}
      {failed && (
        <div className="flex items-center justify-center gap-3 py-5 text-sm text-cyber-text-secondary">
          <span>{t(rows.length ? 'aiCareer.loadMoreFailed' : 'aiCareer.sessionLoadFailed')}</span>
          <button
            type="button"
            onClick={() => void loadPage(true)}
            className="rounded-md border border-cyber-border px-3 py-1 text-cyber-text hover:bg-cyber-text/10"
          >
            {t('aiCareer.retry')}
          </button>
        </div>
      )}
      {(!loading || initial.sessions !== undefined) && !failed && rows.length === 0 && (
        <p className="py-8 text-cyber-text-secondary text-center">{t('aiCareer.noSessions')}</p>
      )}
      {hasMore && !failed && <div ref={sentinelRef} style={{ height: 1 }} />}
      {tip &&
        createPortal(
          <CareerTooltip ref={tipRef} text={tip.text} left={tipLeft} top={tip.top} fixed />,
          document.body
        )}
    </div>
  );
}
