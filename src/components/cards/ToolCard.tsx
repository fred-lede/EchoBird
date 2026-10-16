// ToolCard component
import React from 'react';
import { useI18n } from '../../hooks/useI18n';

export interface ToolCardProps {
  id: string;
  name: string;
  version?: string;
  installed: boolean;
  path?: string;
  detectedPath?: string;
  configPath?: string;

  activeModel?: string;
  website?: string;
  iconBase64?: string;
  names?: Record<string, string>; // i18n names
  command?: string; // CLI command (used by backend for detection)
  hasRemoteInstall?: boolean; // show AI Auto-Install button (driven by remote index.json)
  selected?: boolean;
  onClick?: () => void;
  onMotherAgentInstall?: () => void;
  /** Hide the "版本: …" line. App Manager renders it; "我的AI项目" passes
   *  `actions` (which implicitly hides it too) — so this prop is mostly
   *  reserved for future pages that want a 3-row variant with no actions. */
  hideVersion?: boolean;
  /** When provided, replaces the version row with caller-supplied controls
   *  (typically [Edit] / [Delete] for the "我的AI项目" page). Buttons
   *  inside should call e.stopPropagation() so action clicks don't also
   *  fire the card's onClick. */
  actions?: React.ReactNode;
  /** Override the default icon source. Defaults to ./icons/tools/<id>.svg,
   *  which works for every bundled tool but not for user-added projects on
   *  "我的AI项目" — they pass an absolute file:// path the user picked. */
  iconSrc?: string;
}

export const ToolCard = React.memo(
  ({
    id,
    name,
    version,
    installed,
    path,
    detectedPath,
    configPath,
    activeModel,
    website: _website,
    iconBase64,
    names,
    command: _command,
    hasRemoteInstall,
    selected = false,
    onClick,
    onMotherAgentInstall,
    hideVersion = false,
    actions,
    iconSrc,
  }: ToolCardProps) => {
    const { t, locale } = useI18n();
    const displayName =
      (names &&
        locale !== 'en' &&
        (names[locale] ||
          names[locale.split('-')[0]] ||
          Object.entries(names).find(([k]) => k.startsWith(locale.split('-')[0]))?.[1])) ||
      name;

    // Show AI Auto-Install button based on remote index (not local command field)
    const showMotherInstall = !installed && !!hasRemoteInstall;

    const handleCardClick = () => {
      if (installed) onClick?.();
    };

    // Two visual modes:
    //
    //  Fixed-content (AppManager): icon top-right (right-side = "what is this
    //   tool's branding"), name beneath, version line at the bottom.
    //
    //  Editable-content (MyProjects): icon inline-left of the name (right
    //   side now belongs to the [delete]/[edit] affordance, matching
    //   ModelCard). Triggered automatically when caller passes `actions`.
    const isEditable = !!actions;
    const handleImgError = (e: React.SyntheticEvent<HTMLImageElement>) => {
      const img = e.target as HTMLImageElement;
      // Caller supplied iconSrc — no built-in fallback chain (their path
      // failed, ours wouldn't help). Hide and move on.
      if (iconSrc) {
        img.style.display = 'none';
        return;
      }
      if (img.src.endsWith('.svg')) {
        img.src = `./icons/tools/${id}.png`;
      } else if (!img.src.startsWith('data:') && iconBase64) {
        img.src = iconBase64;
      } else {
        img.style.display = 'none';
      }
    };
    const iconUrl =
      iconSrc ||
      `./icons/tools/${id}.${['dsh', 'grokbot', 'cline', 'clinedesktop', 'antigravity', 'antigravitydesktop', 'musecode'].includes(id) ? 'png' : 'svg'}`;

    return (
      <div
        className={`p-5 min-h-[160px] border bg-cyber-surface ${
          selected
            ? 'border-cyber-accent'
            : `border-transparent ${installed ? 'hover:bg-cyber-elevated' : ''}`
        } relative overflow-hidden rounded-card ${installed ? 'cursor-pointer' : 'cursor-default'} transition-colors flex flex-col`}
        onClick={handleCardClick}
      >
        {isEditable ? (
          <>
            {/* Editable layout — actions occupy the top-right slot, icon
                rides inline-left of the title so the affordance is in the
                same place as ModelCard. */}
            <div className="absolute top-2 right-2 z-10 flex gap-1.5">{actions}</div>
            <div className="flex items-center gap-2 pr-24">
              <img
                src={iconUrl}
                alt={name}
                className={`w-7 h-7 rounded-lg flex-shrink-0 ${selected ? 'opacity-100' : 'opacity-60'}`}
                onError={handleImgError}
              />
              <div
                className={`text-lg font-bold truncate ${installed ? 'text-cyber-text' : 'text-cyber-text-secondary'}`}
              >
                {displayName}
              </div>
            </div>
          </>
        ) : (
          <>
            {/* Fixed layout — icon top-right (App Manager branding slot). */}
            <img
              src={iconUrl}
              alt={name}
              className="absolute top-4 right-4 w-10 h-10 rounded-lg"
              onError={handleImgError}
            />
            <div
              className={`text-lg font-bold truncate pr-12 ${installed ? 'text-cyber-text' : showMotherInstall ? 'text-cyber-text-secondary' : 'text-cyber-text-secondary'}`}
            >
              {displayName}
            </div>
          </>
        )}

        {/* Info rows — always rendered to hold consistent card height;
            invisible (but still occupying space) for CLI-installable tools
            in the AppManager flow that need the install-via-Mother CTA. */}
        <div className="relative mt-3">
          <div
            className={`text-xs space-y-1.5 ${installed ? 'text-cyber-text/60' : 'text-cyber-text-muted/70'} ${showMotherInstall ? 'invisible' : ''}`}
          >
            <div className="truncate">
              {t('tool.models')}: {installed ? activeModel || '-' : '-'}
            </div>
            <div className="truncate">
              {t('tool.app')}: {installed ? detectedPath || path || '-' : '-'}
            </div>
            <div className="truncate">
              {t('tool.config')}: {installed ? configPath || '-' : '-'}
            </div>
            {/* Version line — only when not in editable mode and not
                explicitly suppressed. Editable mode already shows actions
                top-right and doesn't need a 4th info row. */}
            {!isEditable && !hideVersion && (
              <div className="truncate">
                {t('tool.version')}: {installed ? version || '-' : '-'}
              </div>
            )}
          </div>
          {showMotherInstall && (
            <div className="absolute inset-0 flex items-center justify-center">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onMotherAgentInstall?.();
                }}
                className="py-1.5 px-5 text-xs font-bold rounded bg-cyber-elevated text-cyber-text hover:bg-cyber-elevated/80 transition-all"
              >
                {t('agent.installViaMother')}
              </button>
            </div>
          )}
        </div>
      </div>
    );
  }
);
