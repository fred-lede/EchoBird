// Tools store — shared state for detected tools and scanning
// Used by: App.tsx (init), AppManagerProvider, MotherAgentProvider

import { create } from 'zustand';
import * as api from '../api/tauri';
import type { LocalTool } from '../api/types';
import { preloadToolIcons } from '../utils/toolIcons';

interface ToolsState {
  detectedTools: LocalTool[];
  isScanning: boolean;
  hasScanned: boolean;
  setDetectedTools: (tools: LocalTool[] | ((prev: LocalTool[]) => LocalTool[])) => void;
  scanTools: () => Promise<void>;
}

export const useToolsStore = create<ToolsState>((set, get) => ({
  detectedTools: [],
  isScanning: false,
  hasScanned: false,
  setDetectedTools: (tools) =>
    set((state) => ({
      detectedTools: typeof tools === 'function' ? tools(state.detectedTools) : tools,
      hasScanned: true,
    })),

  scanTools: async () => {
    if (get().isScanning) return;
    set({ isScanning: true });
    try {
      const tools = await api.scanTools();
      // Publish one complete desktop; keep the previous icons visible during refresh.
      set({ detectedTools: await preloadToolIcons(tools) });
    } catch {
      /* ignore */
    }
    set({ isScanning: false, hasScanned: true });
  },
}));
