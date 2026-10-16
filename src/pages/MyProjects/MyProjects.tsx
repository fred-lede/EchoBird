// MyProjects page — built-in samples (Reversi / AI Translator) shown
// alongside user-added AI projects in a single grid.
//
// Two-table architecture:
//   • Built-ins live in code + the ~/.echobird/<id>/ reference copy on
//     disk; they are recomputed on every render and never stored in
//     localStorage. They render with the same visual treatment as user
//     entries, but [delete] is hidden (system data — not removable) and
//     [edit] opens a read-only inspector whose 📁 affordances open the
//     reference folder in the system file manager (Tauri shell:open) so
//     users can browse/copy the schema files for learning.
//
//   • User projects live in localStorage via myProjectsStore. CRUD as
//     usual: editable fields, 📁 file picker, [delete] removes the entry.
//
// Launch behaviour is unchanged: linkedToolId on a built-in still routes
// AppManager's selectedTool to the bundled launch_game flow; user entries
// are inert on click for now (Phase D will spawn their launcher exe).
import React, { useState, useEffect, useCallback, useRef } from 'react';
import { Plus, Folder, X } from 'lucide-react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { convertFileSrc } from '@tauri-apps/api/core';
import { useI18n } from '../../hooks/useI18n';
import * as api from '../../api/tauri';
import {
  useMyProjectsStore,
  joinPath,
  pickToolName,
  BUILTIN_TOOL_IDS,
  type MyProject,
  type MyProjectInput,
  type BuiltinToolId,
} from '../../stores/myProjectsStore';
import { useToolsStore } from '../../stores/toolsStore';
import { useAppManager } from '../AppManager/context';
import { ToolCard } from '../../components';
import { useConfirm } from '../../components/ConfirmDialog';
import { useToast } from '../../components/Toast';

// Placeholder examples shown inside the file-picker fields when nothing's
// been chosen yet. Kept in English path style across all locales — the
// example values themselves are filesystem paths, not translatable copy.
const PLACEHOLDER_ICON = 'e.g: ~/YourProject/xxx.ico/svg/png';
const PLACEHOLDER_LAUNCHER = 'e.g: ~/YourProject/xxx.exe / xxx.app / xxx';
const PLACEHOLDER_MODELS = 'e.g: ~/YourProject/models.json';

// Convert any stored icon path into something the WebView can render.
//   - Seeded built-ins: "./icons/tools/<id>.svg" / "../foo" — Vite-served,
//     pass through verbatim.
//   - Web URLs / data URIs: pass through.
//   - User-picked filesystem path (Windows C:\..., macOS/Linux /...): go
//     through Tauri's asset protocol via convertFileSrc(). Bare file://
//     URLs are blocked by the WebView for security; the asset protocol
//     (enabled in tauri.conf.json's app.security.assetProtocol with a
//     "**" scope) is the supported way to render local files as <img src>.
//   - Empty string: caller falls back to a placeholder glyph.
const iconSrcFor = (p: string): string => {
  if (!p) return '';
  if (p.startsWith('./') || p.startsWith('../')) return p;
  if (/^https?:/.test(p) || p.startsWith('data:')) return p;
  return convertFileSrc(p);
};

// ── Card grid + dialog wiring ──

export const MyProjectsMain: React.FC = () => {
  const { t, locale } = useI18n();
  const projects = useMyProjectsStore((s) => s.projects);
  const initStore = useMyProjectsStore((s) => s.init);
  const ensureBuiltinDirs = useMyProjectsStore((s) => s.ensureBuiltinDirs);
  const builtinDirs = useMyProjectsStore((s) => s.builtinDirs);
  const hiddenBuiltins = useMyProjectsStore((s) => s.hiddenBuiltins);
  const setSelectedUserProjectId = useMyProjectsStore((s) => s.setSelectedUserProjectId);
  const detectedTools = useToolsStore((s) => s.detectedTools);
  // Reuse AppManager's selection state. Built-ins set their linkedToolId
  // so the right panel + launch button drive the existing bundled-tool
  // flow. Pure user projects don't have a linkedToolId yet — selecting
  // them currently clears the right panel until Phase D wires their
  // dedicated launch path.
  const { setSelectedTool } = useAppManager();
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);

  // Hydrate localStorage once on mount + strip any legacy seeded entries.
  useEffect(() => {
    initStore();
  }, [initStore]);

  // Resolve each built-in's reference-copy directory in ~/.echobird/<id>/
  // (Rust copies the bundle there on first call, idempotent thereafter).
  useEffect(() => {
    if (detectedTools.length > 0) void ensureBuiltinDirs(detectedTools);
  }, [detectedTools, ensureBuiltinDirs]);

  // Compute built-in entries from code + resolved dirs + live tool scan.
  // Same shape as user projects so the rest of the page treats them
  // uniformly; they're just not persisted.
  const builtinEntries: MyProject[] = BUILTIN_TOOL_IDS.map((id): MyProject | null => {
    if (hiddenBuiltins.includes(id)) return null; // user-hidden
    const dir = builtinDirs[id];
    if (!dir) return null;
    const tool = detectedTools.find((tt) => tt.id === id);
    if (!tool) return null;
    return {
      id: `builtin-${id}`,
      name: pickToolName(tool, locale),
      iconPath: joinPath(dir, `${id}.svg`),
      launcherPath: joinPath(dir, 'game.html'),
      modelsJsonPath: joinPath(dir, 'models.json'),
      createdAt: 0,
      linkedToolId: id,
    };
  }).filter((p): p is MyProject => p !== null);

  const openAdd = () => {
    setEditingId(null);
    setDialogOpen(true);
  };
  const openEdit = (id: string) => {
    setEditingId(id);
    setDialogOpen(true);
  };
  const closeDialog = () => setDialogOpen(false);

  const handleSelect = (project: MyProject) => {
    if (project.linkedToolId) {
      // Built-in — AppManager owns the right side; clear the user-project
      // selection so MyProjectsPanel/Bottom don't double-render.
      setSelectedUserProjectId(null);
      setSelectedTool(project.linkedToolId);
    } else {
      // User project — own the right side via selectedUserProjectId; clear
      // AppManager's selection so its panel/bottom don't overlay.
      setSelectedTool(null);
      setSelectedUserProjectId(project.id);
    }
  };

  // Lookup helper used by the dialog to find the entry's source data
  // (either a built-in computed entry or a user-stored project).
  const findEntry = (id: string): MyProject | undefined =>
    builtinEntries.find((p) => p.id === id) || projects.find((p) => p.id === id);

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="flex-1 overflow-y-auto">
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
          {builtinEntries.map((p) => (
            <ProjectToolCard
              key={p.id}
              project={p}
              isBuiltin
              onSelect={() => handleSelect(p)}
              onEdit={openEdit}
            />
          ))}
          {projects.map((p) => (
            <ProjectToolCard
              key={p.id}
              project={p}
              isBuiltin={false}
              onSelect={() => handleSelect(p)}
              onEdit={openEdit}
            />
          ))}
          <button
            onClick={openAdd}
            className="relative p-5 border border-dashed border-cyber-border rounded-card bg-cyber-surface/40 flex flex-col items-center justify-center min-h-[160px] text-cyber-text-secondary hover:text-cyber-text hover:border-cyber-text/40 hover:bg-cyber-surface transition-colors outline-none"
          >
            <Plus size={28} className="mb-2" />
            <span className="text-[14px] font-medium">{t('myProjects.empty.title')}</span>
            <span className="text-[12px] mt-1 text-cyber-text-muted">
              {t('myProjects.empty.hint')}
            </span>
          </button>
        </div>
      </div>

      {dialogOpen && (
        <AddProjectDialog editingId={editingId} onClose={closeDialog} findEntry={findEntry} />
      )}
    </div>
  );
};

// ── ProjectToolCard ──
// Thin adapter that renders any MyProject — built-in or user — via the
// same AppManager ToolCard. Built-ins hide the [delete] button (they're
// system data, not removable); both keep [edit] but the dialog adapts its
// behaviour based on the entry type.

const ProjectToolCard: React.FC<{
  project: MyProject;
  isBuiltin: boolean;
  onSelect: () => void;
  onEdit: (id: string) => void;
}> = ({ project, isBuiltin, onSelect, onEdit }) => {
  const { t } = useI18n();
  const { showToast } = useToast();
  const detectedTools = useToolsStore((s) => s.detectedTools);
  const deleteProject = useMyProjectsStore((s) => s.deleteProject);
  const hideBuiltin = useMyProjectsStore((s) => s.hideBuiltin);
  const lastAppliedModelInternalId = useMyProjectsStore((s) => s.lastAppliedModelInternalId);
  const { userModels } = useAppManager();
  const confirm = useConfirm();

  // Built-ins read live activeModel off the tool scan so swapping the
  // model from the right panel updates the card in place. User projects
  // don't appear in the tool scan, so we mirror AppManager's optimistic
  // pattern via the store's lastAppliedModelInternalId map — populated
  // on click in MyProjectsBottom regardless of apply outcome.
  const linked = project.linkedToolId
    ? detectedTools.find((tool) => tool.id === project.linkedToolId)
    : undefined;
  const userActiveModelId = project.linkedToolId
    ? undefined
    : lastAppliedModelInternalId[project.id];
  const userActiveModel = userActiveModelId
    ? userModels.find((m) => m.internalId === userActiveModelId)
    : undefined;
  const activeModelDisplay =
    linked?.activeModel || userActiveModel?.modelId || userActiveModel?.name;

  return (
    <ToolCard
      id={project.linkedToolId || project.id}
      iconSrc={project.linkedToolId ? undefined : iconSrcFor(project.iconPath)}
      name={project.name}
      installed
      detectedPath={project.launcherPath}
      configPath={project.modelsJsonPath}
      activeModel={activeModelDisplay}
      onClick={onSelect}
      actions={
        // Bracketed mono text — same visual as ModelCard's [delete] / [edit]
        // in Model Nexus. For built-ins, [delete] hides the entry via a
        // separate localStorage list (`hiddenBuiltins`) — the underlying
        // reference files in ~/.echobird/<id>/ are untouched, and the
        // built-in can be brought back by clearing that list. For user
        // projects, [delete] removes the localStorage entry permanently.
        <div className="flex items-center justify-end gap-2">
          <button
            onClick={async (e) => {
              e.stopPropagation();
              const ok = await confirm({
                title: t('myProjects.deleteTitle'),
                message: t('myProjects.deleteConfirm'),
                confirmText: t('btn.delete'),
                cancelText: t('btn.cancel'),
                type: 'danger',
              });
              if (!ok) return;
              if (isBuiltin && project.linkedToolId) {
                hideBuiltin(project.linkedToolId as BuiltinToolId);
              } else {
                try {
                  await deleteProject(project.id);
                } catch {
                  showToast('error', t('error.requestFailed'));
                }
              }
            }}
            className="text-xs font-mono text-cyber-text-muted/70 hover:text-red-500 transition-colors"
          >
            [{t('btn.delete')}]
          </button>
          <button
            onClick={(e) => {
              e.stopPropagation();
              onEdit(project.id);
            }}
            className="text-xs font-mono text-cyber-text-muted/70 hover:text-cyber-text transition-colors"
          >
            [{t('btn.edit')}]
          </button>
        </div>
      }
    />
  );
};

// ── Add / Edit dialog ──

const AddProjectDialog: React.FC<{
  editingId: string | null;
  onClose: () => void;
  findEntry: (id: string) => MyProject | undefined;
}> = ({ editingId, onClose, findEntry }) => {
  const { t } = useI18n();
  const { showToast } = useToast();
  const addProject = useMyProjectsStore((s) => s.addProject);
  const updateProject = useMyProjectsStore((s) => s.updateProject);
  const builtinDirs = useMyProjectsStore((s) => s.builtinDirs);

  // Initial values: empty (Add) or existing entry (Edit, built-in or user).
  const existing = editingId ? findEntry(editingId) : undefined;
  const isBuiltin = !!existing?.linkedToolId;
  // Folder to open when a built-in's 📁 is clicked — same dir for all
  // three file fields (everything lives under ~/.echobird/<id>/).
  const builtinFolder = isBuiltin
    ? builtinDirs[existing!.linkedToolId as BuiltinToolId]
    : undefined;

  const [name, setName] = useState(existing?.name ?? '');
  const [iconPath, setIconPath] = useState(existing?.iconPath ?? '');
  const [launcherPath, setLauncherPath] = useState(existing?.launcherPath ?? '');
  const [modelsJsonPath, setModelsJsonPath] = useState(existing?.modelsJsonPath ?? '');
  const [saving, setSaving] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // ESC closes
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [onClose]);

  // Pass currentValue so the OS dialog opens at that file's directory when
  // the user already has a path picked. Skip for Vite-served URLs.
  const looksLikeAbsolutePath = (p: string): boolean => {
    if (!p) return false;
    if (p.startsWith('./') || p.startsWith('../')) return false;
    if (/^https?:/.test(p) || p.startsWith('data:')) return false;
    if (/^[A-Za-z]:[\\/]/.test(p) || p.startsWith('\\\\')) return true;
    if (p.startsWith('/')) return true;
    return false;
  };

  const pickFile = useCallback(
    async (
      filters: { name: string; extensions: string[] }[],
      setter: (v: string) => void,
      currentValue: string
    ) => {
      try {
        const opts: { multiple: false; filters: typeof filters; defaultPath?: string } = {
          multiple: false,
          filters,
        };
        if (looksLikeAbsolutePath(currentValue)) opts.defaultPath = currentValue;
        const result = await openDialog(opts);
        if (typeof result === 'string') setter(result);
      } catch (e) {
        console.error('[MyProjects] file picker failed:', e);
      }
    },
    []
  );

  // For built-ins, 📁 opens the reference-copy folder in the system file
  // manager so users can browse / read / copy the schema files.
  const openBuiltinFolder = useCallback(async () => {
    if (!builtinFolder) return;
    try {
      await api.openFolder(builtinFolder);
    } catch (e) {
      console.error('[MyProjects] open folder failed:', e);
    }
  }, [builtinFolder]);

  const handleSave = useCallback(async () => {
    if (saving) return;
    if (isBuiltin) {
      // Built-ins are read-only; the dialog acts as an inspector. No save.
      onClose();
      return;
    }
    const input: MyProjectInput = {
      name: name.trim(),
      iconPath,
      launcherPath,
      modelsJsonPath,
    };
    setSaving(true);
    try {
      if (editingId) {
        await updateProject(editingId, input);
      } else {
        await addProject(input);
      }
      if (mounted.current) onClose();
    } catch {
      showToast('error', t('error.requestFailed'));
    } finally {
      if (mounted.current) setSaving(false);
    }
  }, [
    isBuiltin,
    name,
    iconPath,
    launcherPath,
    modelsJsonPath,
    editingId,
    updateProject,
    addProject,
    onClose,
    saving,
    showToast,
    t,
  ]);

  return (
    <div className="fixed inset-0 z-[9998] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60" onClick={onClose} />
      <div
        className="relative w-[480px] max-w-[92vw] border border-cyber-border/40 bg-cyber-surface shadow-2xl rounded-xl overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="h-px w-full bg-cyber-border" />
        <div className="px-6 pt-5 pb-4 flex items-center justify-between">
          <span className="text-lg font-bold text-cyber-text font-mono">
            &gt;_ {t('myProjects.dialog.title')}
          </span>
          <button
            onClick={onClose}
            className="text-cyber-text-secondary hover:text-cyber-text transition-colors"
          >
            <X size={18} />
          </button>
        </div>

        <div className="px-6 pb-6 space-y-4">
          <FieldLabel label={t('myProjects.field.name')}>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t('myProjects.placeholder.name')}
              readOnly={isBuiltin}
              className={`w-full px-3 py-2 border rounded text-[14px] transition-colors outline-none ${
                isBuiltin
                  ? 'bg-cyber-input/40 border-cyber-border/60 text-cyber-text-muted cursor-not-allowed'
                  : 'bg-cyber-input border-cyber-border text-cyber-text placeholder:text-cyber-text-muted focus:border-cyber-text/40'
              }`}
            />
          </FieldLabel>

          <FieldLabel label={t('myProjects.field.icon')}>
            <FilePickerButton
              value={iconPath}
              placeholder={PLACEHOLDER_ICON}
              onClick={
                isBuiltin
                  ? openBuiltinFolder
                  : () =>
                      pickFile(
                        [{ name: 'Icon', extensions: ['ico', 'svg', 'png'] }],
                        setIconPath,
                        iconPath
                      )
              }
            />
          </FieldLabel>

          <FieldLabel label={t('myProjects.field.launcher')}>
            <FilePickerButton
              value={launcherPath}
              placeholder={PLACEHOLDER_LAUNCHER}
              onClick={
                isBuiltin
                  ? openBuiltinFolder
                  : () =>
                      // No extension filter — only Windows uses .exe, macOS
                      // is .app, Linux is arbitrary. Let the user pick
                      // anything and accept that an invalid pick fails on
                      // launch.
                      pickFile([], setLauncherPath, launcherPath)
              }
            />
          </FieldLabel>

          <FieldLabel label={t('myProjects.field.models')}>
            <FilePickerButton
              value={modelsJsonPath}
              placeholder={PLACEHOLDER_MODELS}
              onClick={
                isBuiltin
                  ? openBuiltinFolder
                  : () =>
                      pickFile(
                        [{ name: 'models.json', extensions: ['json'] }],
                        setModelsJsonPath,
                        modelsJsonPath
                      )
              }
            />
          </FieldLabel>
        </div>

        <div className="flex border-t border-cyber-border/40">
          {/* Built-ins are read-only — single Close button takes the whole
              row. User projects get Cancel + Save side by side. */}
          {isBuiltin ? (
            <button
              onClick={onClose}
              className="flex-1 px-6 py-3 text-[14px] text-cyber-text hover:bg-cyber-elevated transition-colors font-semibold"
            >
              {t('btn.close')}
            </button>
          ) : (
            <>
              <button
                onClick={onClose}
                className="flex-1 px-6 py-3 text-[14px] text-cyber-text-secondary hover:text-cyber-text hover:bg-cyber-elevated transition-colors"
              >
                {t('btn.cancel')}
              </button>
              <div className="w-px bg-cyber-border/40" />
              <button
                onClick={handleSave}
                disabled={saving}
                className="flex-1 px-6 py-3 text-[14px] text-cyber-text hover:bg-cyber-elevated transition-colors font-semibold"
              >
                {t('btn.save')}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
};

// ── Small helpers ──

const FieldLabel: React.FC<{ label: string; children: React.ReactNode }> = ({
  label,
  children,
}) => (
  <div className="space-y-1.5">
    <div className="text-[13px] text-cyber-text-secondary font-medium">{label}</div>
    {children}
  </div>
);

// File-picker / folder-opener button. Always renders the same visual (input
// box + folder icon on the right) regardless of mode; the caller decides
// what onClick does — file picker for editable rows, "open folder" for
// built-in read-only rows.
const FilePickerButton: React.FC<{
  value: string;
  placeholder: string;
  onClick: () => void;
}> = ({ value, placeholder, onClick }) => (
  <button
    type="button"
    onClick={onClick}
    className="w-full px-3 py-2 bg-cyber-input border border-cyber-border rounded text-[14px] text-left flex items-center justify-between gap-2 hover:border-cyber-text/40 transition-colors outline-none"
  >
    <span className={`truncate ${value ? 'text-cyber-text' : 'text-cyber-text-muted'}`}>
      {value || placeholder}
    </span>
    <Folder size={14} className="flex-shrink-0 text-cyber-text-secondary" />
  </button>
);
