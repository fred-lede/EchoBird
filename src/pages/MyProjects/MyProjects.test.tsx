import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { MyProjectsMain } from './MyProjects';

const mocks = vi.hoisted(() => ({
  projects: [] as Array<{
    id: string;
    name: string;
    iconPath: string;
    launcherPath: string;
    modelsJsonPath: string;
    createdAt: number;
  }>,
  addProject: vi.fn(),
  updateProject: vi.fn(),
  deleteProject: vi.fn(),
  init: vi.fn(),
  showToast: vi.fn(),
  confirm: vi.fn(),
}));

vi.mock('../../hooks/useI18n', () => ({
  useI18n: () => ({ t: (key: string) => key, locale: 'en' }),
}));
vi.mock('../../components/Toast', () => ({ useToast: () => ({ showToast: mocks.showToast }) }));
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => mocks.confirm }));
vi.mock('../../components', () => ({
  ToolCard: ({ name, actions }: { name: string; actions: React.ReactNode }) => (
    <div>
      {name}
      {actions}
    </div>
  ),
}));
vi.mock('../AppManager/context', () => ({
  useAppManager: () => ({ setSelectedTool: vi.fn(), userModels: [] }),
}));
vi.mock('../../stores/toolsStore', () => ({
  useToolsStore: (select: (state: { detectedTools: [] }) => unknown) =>
    select({ detectedTools: [] }),
}));
vi.mock('../../stores/myProjectsStore', () => ({
  BUILTIN_TOOL_IDS: [],
  joinPath: (dir: string, file: string) => `${dir}/${file}`,
  pickToolName: () => '',
  useMyProjectsStore: (select: (state: Record<string, unknown>) => unknown) =>
    select({
      projects: mocks.projects,
      builtinDirs: {},
      hiddenBuiltins: [],
      lastAppliedModelInternalId: {},
      init: mocks.init,
      ensureBuiltinDirs: vi.fn(),
      setSelectedUserProjectId: vi.fn(),
      addProject: mocks.addProject,
      updateProject: mocks.updateProject,
      deleteProject: mocks.deleteProject,
      hideBuiltin: vi.fn(),
    }),
}));

let renderer: ReactTestRenderer | undefined;

function text(node: ReactTestInstance): string {
  return node.children.map((child) => (typeof child === 'string' ? child : text(child))).join('');
}

function button(label: string) {
  return renderer!.root.findAllByType('button').find((node) => text(node) === label)!;
}

function openAdd() {
  const add = renderer!.root
    .findAllByType('button')
    .find((node) => text(node).includes('myProjects.empty.title'))!;
  act(() => add.props.onClick());
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.projects = [];
  mocks.addProject.mockResolvedValue(undefined);
  mocks.updateProject.mockResolvedValue(undefined);
  mocks.deleteProject.mockResolvedValue(undefined);
  mocks.init.mockResolvedValue(undefined);
  mocks.confirm.mockResolvedValue(true);
  vi.stubGlobal('window', { addEventListener: vi.fn(), removeEventListener: vi.fn() });
  act(() => {
    renderer = create(<MyProjectsMain />);
  });
});

afterEach(() => {
  act(() => renderer?.unmount());
  renderer = undefined;
  vi.unstubAllGlobals();
});

it('does not let an old save close a newly opened dialog and discard its draft', async () => {
  let finish: (() => void) | undefined;
  mocks.addProject.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      })
  );
  openAdd();
  let save!: Promise<void>;
  act(() => {
    save = button('btn.save').props.onClick();
  });
  expect(finish).toBeDefined();
  act(() => button('btn.cancel').props.onClick());
  openAdd();
  act(() =>
    renderer!.root.findAllByType('input')[0].props.onChange({ target: { value: 'New draft' } })
  );
  await act(async () => {
    finish!();
    await save;
  });
  expect(renderer!.root.findAllByType('input')[0]?.props.value).toBe('New draft');
});

it('closes the current dialog after its successful save', async () => {
  openAdd();
  await act(async () => {
    await button('btn.save').props.onClick();
  });
  expect(mocks.addProject).toHaveBeenCalledOnce();
  expect(renderer!.root.findAllByType('input')).toHaveLength(0);
});

it('shows an explicit save failure and keeps the dialog draft', async () => {
  mocks.addProject.mockRejectedValueOnce(new Error('disk is full'));
  openAdd();
  act(() =>
    renderer!.root.findAllByType('input')[0].props.onChange({ target: { value: 'Keep draft' } })
  );
  await act(async () => {
    await button('btn.save').props.onClick();
  });
  expect(mocks.showToast).toHaveBeenCalledWith('error', 'error.requestFailed');
  expect(renderer!.root.findAllByType('input')[0].props.value).toBe('Keep draft');
});

it('shows an explicit delete failure after confirmation', async () => {
  mocks.projects = [
    {
      id: 'one',
      name: 'One',
      iconPath: '',
      launcherPath: '/run',
      modelsJsonPath: '/models.json',
      createdAt: 1,
    },
  ];
  mocks.deleteProject.mockRejectedValueOnce(new Error('read-only registry'));
  act(() => renderer!.update(<MyProjectsMain />));
  await act(async () => {
    await button('[btn.delete]').props.onClick({ stopPropagation: vi.fn() });
  });
  expect(mocks.confirm).toHaveBeenCalledOnce();
  expect(mocks.deleteProject).toHaveBeenCalledWith('one');
  expect(mocks.showToast).toHaveBeenCalledWith('error', 'error.requestFailed');
});
