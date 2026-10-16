import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  getMyProjects: vi.fn(),
  saveMyProjects: vi.fn(),
  seedBuiltinToUserDir: vi.fn(),
}));
vi.mock('../api/tauri', () => api);

const project = (id: string) => ({
  id,
  name: id,
  iconPath: '',
  launcherPath: '/fixture/launch',
  modelsJsonPath: '/fixture/models.json',
  createdAt: 1,
});
let storage: Map<string, string>;

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  api.getMyProjects.mockResolvedValue([project('existing')]);
  api.saveMyProjects.mockResolvedValue(undefined);
  storage = new Map();
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  });
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function store() {
  return (await import('./myProjectsStore')).useMyProjectsStore;
}

it('keeps cached projects and legacy data when a subsequent read fails without writing', async () => {
  const state = await store();
  await state.getState().init();
  storage.set('echobird_my_projects', JSON.stringify([project('legacy')]));
  api.getMyProjects.mockRejectedValue(new Error('unreadable registry'));
  await state.getState().init();
  expect(state.getState().projects).toEqual([project('existing')]);
  expect(api.saveMyProjects).not.toHaveBeenCalled();
  expect(storage.has('echobird_my_projects')).toBe(true);
});

it('refuses writes after an unsuccessful initial read and recovers after a successful retry', async () => {
  const state = await store();
  api.getMyProjects.mockRejectedValue(new Error('unreadable registry'));
  await state.getState().init();
  await expect(
    Promise.resolve().then(() => state.getState().addProject(project('new')))
  ).rejects.toThrow();
  expect(api.saveMyProjects).not.toHaveBeenCalled();
  expect(state.getState().projects).toEqual([]);
  api.getMyProjects.mockResolvedValue([project('existing')]);
  await state.getState().init();
  await state.getState().addProject(project('new'));
  expect(state.getState().projects.map((item) => item.name)).toEqual(['existing', 'new']);
});

it('does not discard a registry containing malformed entries before a later save', async () => {
  const state = await store();
  api.getMyProjects.mockResolvedValue([project('existing'), { name: 'unreadable entry' }]);
  await state.getState().init();
  await expect(
    Promise.resolve().then(() => state.getState().deleteProject('existing'))
  ).rejects.toThrow();
  expect(api.saveMyProjects).not.toHaveBeenCalled();
});

it('reports explicit save failures and retains the last saved project list', async () => {
  const state = await store();
  await state.getState().init();
  api.saveMyProjects.mockRejectedValue(new Error('disk is full'));
  await expect(
    Promise.resolve().then(() => state.getState().updateProject('existing', { name: 'changed' }))
  ).rejects.toThrow('disk is full');
  expect(state.getState().projects).toEqual([project('existing')]);
});

it('serializes concurrent project changes so both saved additions survive', async () => {
  const state = await store();
  await state.getState().init();
  let release: (() => void) | undefined;
  api.saveMyProjects.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        release = resolve;
      })
  );
  const first = state.getState().addProject(project('first'));
  const second = state.getState().addProject(project('second'));
  await vi.waitFor(() => expect(release).toBeDefined());
  expect(api.saveMyProjects).toHaveBeenCalledTimes(1);
  release!();
  await Promise.all([first, second]);
  expect(api.saveMyProjects).toHaveBeenLastCalledWith(
    expect.arrayContaining([
      expect.objectContaining({ name: 'existing' }),
      expect.objectContaining({ name: 'first' }),
      expect.objectContaining({ name: 'second' }),
    ])
  );
});

it('only removes the legacy copy after successfully migrating an empty valid registry', async () => {
  const state = await store();
  api.getMyProjects.mockResolvedValue([]);
  storage.set('echobird_my_projects', JSON.stringify([project('legacy')]));
  api.saveMyProjects.mockRejectedValueOnce(new Error('disk is full'));
  await state.getState().init();
  expect(storage.has('echobird_my_projects')).toBe(true);
  await state.getState().init();
  expect(storage.has('echobird_my_projects')).toBe(false);
  expect(state.getState().projects).toEqual([project('legacy')]);
});

it('finishes a failed legacy migration after a successful deletion without resurrecting it', async () => {
  const state = await store();
  let disk: ReturnType<typeof project>[] = [];
  api.getMyProjects.mockImplementation(async () => disk);
  api.saveMyProjects
    .mockImplementation(async (projects: ReturnType<typeof project>[]) => {
      disk = projects;
    })
    .mockRejectedValueOnce(new Error('disk is full'));
  storage.set('echobird_my_projects', JSON.stringify([project('legacy')]));
  await state.getState().init();
  expect(state.getState().projects).toEqual([project('legacy')]);
  expect(storage.has('echobird_my_projects')).toBe(true);
  await state.getState().deleteProject('legacy');
  expect(storage.has('echobird_my_projects')).toBe(false);
  await state.getState().init();
  expect(state.getState().projects).toEqual([]);
  expect(disk).toEqual([]);
});
