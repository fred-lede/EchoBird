import { useLayoutEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../../api/tauri';
import type { GgufFile } from '../../api/types';
import { LocalServerMain, LocalServerPanel, LocalServerProvider } from './LocalServer';
import { useLocalServer } from './context';

const { downloads, startDownload, confirm } = vi.hoisted(() => ({
  downloads: new Map(),
  startDownload: vi.fn(),
  confirm: vi.fn(),
}));
vi.mock('../../hooks/useI18n', () => ({ useI18n: () => ({ t: (key: string) => key }) }));
vi.mock('../../hooks/useDocumentVisible', () => ({ useDocumentVisible: () => false }));
vi.mock('../../components/ConfirmDialog', () => ({ useConfirm: () => confirm }));
vi.mock('../../components/DownloadContext', () => ({
  useDownload: () => ({ downloads, startDownload, isDownloading: false }),
}));
vi.mock('../../components/MiniSelect', () => ({
  MiniSelect: (props: { value: string; onChange: (value: string) => void }) => (
    <select value={props.value} onChange={(e) => props.onChange(e.target.value)} />
  ),
}));
vi.mock('../../api/tauri', () => ({
  getModelsDirs: vi.fn(),
  scanGgufFiles: vi.fn(),
  scanHfModels: vi.fn(),
  getDownloadDir: vi.fn(),
  getGpuInfo: vi.fn(),
  detectGpu: vi.fn(),
  getSystemInfo: vi.fn(),
  getLocalEngineStatus: vi.fn(),
  getLlmCustomCommand: vi.fn(),
  listEngineReleaseOptions: vi.fn(),
  getStoreModels: vi.fn(),
  setDownloadDir: vi.fn(),
  installLocalEngine: vi.fn(),
  downloadModel: vi.fn(),
  startLlmServer: vi.fn(),
  setLlmCustomCommand: vi.fn(),
}));

describe('Local model page navigation', () => {
  let renderer: ReactTestRenderer;
  let context: ReturnType<typeof useLocalServer>;
  function Harness({ main, panel }: { main: boolean; panel: boolean }) {
    const state = useLocalServer();
    useLayoutEffect(() => {
      context = state;
    });
    return (
      <>
        {main && <LocalServerMain />}
        {panel && <LocalServerPanel />}
      </>
    );
  }
  async function open(main = false, panel = false) {
    await act(async () => {
      renderer = create(
        <LocalServerProvider>
          <Harness main={main} panel={panel} />
        </LocalServerProvider>
      );
    });
  }
  function click(label: string) {
    const button = renderer.root.findAllByType('button').find((b) => b.children.includes(label));
    expect(button, label).toBeDefined();
    return button!.props.onClick();
  }
  beforeEach(() => {
    vi.clearAllMocks();
    downloads.clear();
    vi.stubGlobal('navigator', { platform: 'Linux' });
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('Offline')));
    vi.mocked(api.getModelsDirs).mockResolvedValue(['/models']);
    vi.mocked(api.scanGgufFiles).mockResolvedValue([]);
    vi.mocked(api.scanHfModels).mockResolvedValue([]);
    vi.mocked(api.getDownloadDir).mockResolvedValue('/models');
    vi.mocked(api.getGpuInfo).mockResolvedValue(null);
    vi.mocked(api.detectGpu).mockResolvedValue(null);
    vi.mocked(api.getSystemInfo).mockResolvedValue({
      os: 'linux',
      arch: 'x86_64',
      hasNvidiaGpu: true,
      gpuName: 'NVIDIA',
      gpuVramGb: 24,
    });
    vi.mocked(api.getLocalEngineStatus).mockImplementation(async (runtime) => ({
      engines: [{ name: runtime!, installed: true, version: 'b12000', latestVersion: 'b11553' }],
    }));
    vi.mocked(api.getLlmCustomCommand).mockResolvedValue(null);
    vi.mocked(api.getStoreModels).mockResolvedValue([]);
    vi.mocked(api.setDownloadDir).mockResolvedValue('/downloads');
  });
  afterEach(() => {
    act(() => renderer?.unmount());
    vi.unstubAllGlobals();
  });

  it('positive control: scans local models on entry without installing or downloading', async () => {
    await open(true, true);
    expect(api.scanGgufFiles).toHaveBeenCalledWith('/models');
    expect(api.scanHfModels).toHaveBeenCalledWith('/models');
    expect(api.installLocalEngine).not.toHaveBeenCalled();
    expect(api.downloadModel).not.toHaveBeenCalled();
    expect(api.startLlmServer).not.toHaveBeenCalled();
    expect(api.setLlmCustomCommand).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });

  it('keeps a manually selected llama.cpp runtime on Linux', async () => {
    await open(true);
    expect(context.runtime).toBe('vllm');
    await act(async () => {
      context.setRuntime('llama-server');
    });
    expect(context.runtime).toBe('llama-server');
  });

  it('clears the selected model and rescans the correct format after switching runtimes', async () => {
    await open();
    act(() => context.setSelectedModelPath('/models/model.gguf'));
    await act(async () => {
      context.setRuntime('vllm');
    });
    expect(context.selectedModelPath).toBeNull();
    expect(api.scanHfModels).toHaveBeenCalledWith('/models');
  });

  it('ignores an older in-flight model scan after a runtime switch', async () => {
    let resolveOld!: (files: GgufFile[]) => void;
    vi.mocked(api.scanGgufFiles).mockReturnValueOnce(
      new Promise((resolve) => {
        resolveOld = resolve;
      })
    );
    vi.mocked(api.scanHfModels).mockResolvedValue([
      { modelName: 'New HF', modelPath: '/models/new', totalSize: 123 },
    ]);
    await open();
    await act(async () => {
      context.setRuntime('vllm');
    });
    await act(async () => {
      resolveOld([{ fileName: 'old.gguf', filePath: '/models/old.gguf', fileSize: 20 }]);
    });
    expect(context.ggufFiles.map((f) => f.fileName)).toEqual(['New HF']);
    expect(context.isScanning).toBe(false);
  });

  it('does not expose cached GGUFs when the new HF scan fails', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.scanGgufFiles).mockResolvedValue([
      { fileName: 'old.gguf', filePath: '/models/old.gguf', fileSize: 20 },
    ]);
    vi.mocked(api.scanHfModels).mockRejectedValue(new Error('Scan failed'));
    await open();
    expect(context.ggufFiles).toHaveLength(1);
    await act(async () => {
      context.setRuntime('vllm');
    });
    expect(context.ggufFiles).toEqual([]);
    expect(context.isScanning).toBe(false);
    vi.restoreAllMocks();
  });

  it('loads the bundled catalog offline without a missing static-file fetch', async () => {
    await open(false, true);
    await act(async () => {
      click('server.store');
    });
    expect(api.getStoreModels).toHaveBeenCalledOnce();
    expect(renderer.root.findAllByType('img').length).toBeGreaterThan(0);
    expect(fetch).not.toHaveBeenCalled();
    expect(startDownload).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });

  it('puts small downloadable models first even when the remote catalog starts with flagships', async () => {
    const remoteModels = (
      [
        ['flagship', 600e9, ['llama-server']],
        ['large', 12e9, ['llama-server']],
        ['small', 3e9, ['llama-server']],
        ['other-runtime', 1e8, ['vllm']],
        ['unknown', 0, ['llama-server']],
      ] as [string, number, string[]][]
    ).map(([id, fileSize, runtimes]) => ({
      id,
      name: id,
      icon: 'qwen',
      description: '',
      huggingfaceRepo: `publisher/${id}`,
      runtimes,
      variants: [
        {
          quantization: 'Q8_0',
          fileSize,
          recommendedVRAM: '',
          files: [`${id}-Q8_0.gguf`],
        },
      ],
    }));
    remoteModels[1].variants.push({
      quantization: 'Q4_K_M',
      fileSize: 2e9,
      recommendedVRAM: '',
      files: ['large-Q4_K_M.gguf'],
    });
    vi.mocked(api.getStoreModels).mockResolvedValue(remoteModels);
    await open(false, true);
    await act(async () => {
      click('server.store');
    });
    expect(renderer.root.findAllByType('img').map((img) => img.props.alt)).toEqual([
      'large',
      'small',
      'flagship',
      'unknown',
    ]);
    expect(remoteModels.map((model) => model.id)).toEqual([
      'flagship',
      'large',
      'small',
      'other-runtime',
      'unknown',
    ]);
    expect(api.getStoreModels).toHaveBeenCalledOnce();
    expect(startDownload).not.toHaveBeenCalled();
    expect(api.installLocalEngine).not.toHaveBeenCalled();
    expect(api.startLlmServer).not.toHaveBeenCalled();
    expect(api.setDownloadDir).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });

  it('keeps a downloaded distill separate from its base model when their filename stems match', async () => {
    const models = [
      ['qwen-base', 'Qwen3.5 2B', 'Qwen3.5-2B-Q4_K_M.gguf'],
      ['qwen-distill', 'Qwen3.5 2B Opus Distill', 'Qwen3.5-2B.Q4_K_M.gguf'],
    ].map(([id, name, file]) => ({
      id,
      name,
      icon: 'qwen',
      description: '',
      huggingfaceRepo: `publisher/${id}`,
      runtimes: ['llama-server'],
      variants: [{ quantization: 'Q4_K_M', files: [file], fileSize: 100, recommendedVRAM: '4 GB' }],
    }));
    vi.mocked(api.getStoreModels).mockResolvedValue(models);
    vi.mocked(api.scanGgufFiles).mockResolvedValue(
      models.map((model) => ({
        fileName: model.variants[0].files[0],
        filePath: `/models/${model.variants[0].files[0]}`,
        fileSize: 100,
      }))
    );
    await open(false, true);
    await act(async () => {
      click('server.store');
    });
    act(() => click('server.local'));
    expect(renderer.root.findAllByType('img').map((img) => img.props.alt)).toEqual(
      models.map((model) => model.name)
    );
    expect(context.selectedModelPath).toBeNull();
    expect(api.getStoreModels).toHaveBeenCalledOnce();
    expect(api.setDownloadDir).not.toHaveBeenCalled();
    expect(api.installLocalEngine).not.toHaveBeenCalled();
    expect(api.startLlmServer).not.toHaveBeenCalled();
    expect(startDownload).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });

  it.each(['paused', 'downloading'] as const)(
    'keeps the directory pinned while a model is %s',
    async (status) => {
      downloads.set('model.gguf', { status, repo: 'publisher/model', files: ['model.gguf'] });
      await open(false, true);
      await act(async () => {
        click('server.store');
      });
      const directory = renderer.root.find(
        (node) => node.type === 'div' && node.children.includes('download.location')
      );
      await act(async () => {
        await directory.props.onClick();
      });
      expect(api.setDownloadDir).not.toHaveBeenCalled();
    }
  );

  it('positive control: changes the download directory when no model job is pending', async () => {
    await open(false, true);
    await act(async () => {
      click('server.store');
    });
    const directory = renderer.root.find(
      (node) => node.type === 'div' && node.children.includes('download.location')
    );
    await act(async () => {
      await directory.props.onClick();
    });
    expect(api.setDownloadDir).toHaveBeenCalledOnce();
    expect(JSON.stringify(renderer.toJSON())).toContain('/downloads');
    expect(api.scanGgufFiles).toHaveBeenCalledTimes(2);
  });

  it('rescans the current runtime when an earlier directory picker returns after a switch', async () => {
    let resolveDirectory!: (directory: string) => void;
    vi.mocked(api.setDownloadDir).mockReturnValueOnce(
      new Promise((resolve) => {
        resolveDirectory = resolve;
      })
    );
    vi.mocked(api.scanGgufFiles).mockResolvedValue([
      { fileName: 'old.gguf', filePath: '/models/old.gguf', fileSize: 20 },
    ]);
    vi.mocked(api.scanHfModels).mockResolvedValue([
      { modelName: 'Current HF', modelPath: '/models/current', totalSize: 123 },
    ]);
    await open(false, true);
    await act(async () => {
      click('server.store');
    });
    const directory = renderer.root.find(
      (node) => node.type === 'div' && node.children.includes('download.location')
    );
    let directoryChange!: Promise<void>;
    act(() => {
      directoryChange = directory.props.onClick();
    });
    await act(async () => {
      context.setRuntime('vllm');
    });
    await act(async () => {
      resolveDirectory('/downloads');
      await directoryChange;
    });
    expect(context.runtime).toBe('vllm');
    expect(context.ggufFiles.map((file) => file.fileName)).toEqual(['Current HF']);
    expect(api.scanGgufFiles).toHaveBeenCalledOnce();
    expect(api.scanHfModels).toHaveBeenCalledTimes(2);
    expect(api.setDownloadDir).toHaveBeenCalledOnce();
    expect(startDownload).not.toHaveBeenCalled();
    expect(api.installLocalEngine).not.toHaveBeenCalled();
    expect(api.startLlmServer).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });

  it('honors an explicit scan format after the default runtime changes', async () => {
    await open();
    await act(async () => {
      context.setRuntime('vllm');
    });
    vi.clearAllMocks();
    await act(async () => {
      await context.rescanModels('llama-server');
    });
    expect(api.scanGgufFiles).toHaveBeenCalledWith('/models');
    expect(api.scanHfModels).not.toHaveBeenCalled();
    expect(context.runtime).toBe('vllm');
  });

  it('does not offer a downgrade as an engine update', async () => {
    vi.mocked(api.getSystemInfo).mockResolvedValue({
      os: 'windows',
      arch: 'x86_64',
      hasNvidiaGpu: false,
      gpuName: null,
      gpuVramGb: null,
    });
    await open(true);
    expect(
      renderer.root.findAllByType('button').some((b) => b.children.includes('server.upgradeEngine'))
    ).toBe(false);
  });

  it('positive control: offers an actual newer engine build', async () => {
    vi.mocked(api.getSystemInfo).mockResolvedValue({
      os: 'windows',
      arch: 'x86_64',
      hasNvidiaGpu: false,
      gpuName: null,
      gpuVramGb: null,
    });
    vi.mocked(api.getLocalEngineStatus).mockResolvedValue({
      engines: [
        { name: 'llama-server', installed: true, version: 'b11552', latestVersion: 'b11553' },
      ],
    });
    await open(true);
    expect(
      renderer.root.findAllByType('button').some((b) => b.children.includes('server.upgradeEngine'))
    ).toBe(true);
  });

  it('rechecks the retained engine after an upgrade fails', async () => {
    vi.mocked(api.getSystemInfo).mockResolvedValue({
      os: 'windows',
      arch: 'x86_64',
      hasNvidiaGpu: false,
      gpuName: null,
      gpuVramGb: null,
    });
    vi.mocked(api.getLocalEngineStatus).mockResolvedValue({
      engines: [
        { name: 'llama-server', installed: true, version: 'b11552', latestVersion: 'b11553' },
      ],
    });
    vi.mocked(api.installLocalEngine).mockRejectedValueOnce(new Error('Download failed'));
    await open(true);
    await act(async () => {
      await click('server.upgradeEngine');
    });
    expect(api.getLocalEngineStatus).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(renderer.toJSON())).toContain('b11552');
    expect(JSON.stringify(renderer.toJSON())).toContain('Download failed');
    expect(
      renderer.root.findAllByType('button').some((b) => b.children.includes('server.upgradeEngine'))
    ).toBe(true);
  });

  it('only lists a complete shard group from one directory as a local model', async () => {
    vi.mocked(api.scanGgufFiles).mockResolvedValue([
      {
        fileName: 'Complete-Q4-00001-of-00002.gguf',
        filePath: '/models/a/Complete-Q4-00001-of-00002.gguf',
        fileSize: 10,
      },
      {
        fileName: 'Complete-Q4-00002-of-00002.gguf',
        filePath: '/models/a/Complete-Q4-00002-of-00002.gguf',
        fileSize: 10,
      },
      {
        fileName: 'Incomplete-Q4-00002-of-00002.gguf',
        filePath: '/models/Incomplete-Q4-00002-of-00002.gguf',
        fileSize: 10,
      },
      {
        fileName: 'Mixed-Q4-00001-of-00002.gguf',
        filePath: '/models/a/Mixed-Q4-00001-of-00002.gguf',
        fileSize: 10,
      },
      {
        fileName: 'Mixed-Q4-00002-of-00002.gguf',
        filePath: '/models/b/Mixed-Q4-00002-of-00002.gguf',
        fileSize: 10,
      },
    ]);
    await open(false, true);
    const output = JSON.stringify(renderer.toJSON());
    expect(output).toContain('Complete');
    expect(output).not.toContain('Incomplete');
    expect(output).not.toContain('Mixed');
  });

  it('does not mark cross-directory shards as a downloaded store variant', async () => {
    vi.mocked(api.getStoreModels).mockResolvedValue([
      {
        id: 'split',
        name: 'Split',
        icon: 'qwen',
        description: '',
        huggingfaceRepo: 'publisher/model',
        runtimes: ['llama-server'],
        variants: [
          {
            quantization: 'Q4',
            fileSize: 20,
            recommendedVRAM: '1 GB',
            files: ['Split-Q4-00001-of-00002.gguf', 'Split-Q4-00002-of-00002.gguf'],
          },
        ],
      },
    ]);
    vi.mocked(api.scanGgufFiles).mockResolvedValue([
      {
        fileName: 'Split-Q4-00001-of-00002.gguf',
        filePath: '/models/a/Split-Q4-00001-of-00002.gguf',
        fileSize: 10,
      },
      {
        fileName: 'Split-Q4-00002-of-00002.gguf',
        filePath: '/models/b/Split-Q4-00002-of-00002.gguf',
        fileSize: 10,
      },
    ]);
    await open(false, true);
    await act(async () => {
      click('server.store');
    });
    expect(JSON.stringify(renderer.toJSON())).not.toContain('store.ready');
  });
});
