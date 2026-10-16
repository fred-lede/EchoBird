import { create } from 'zustand';

// Keep the plain boolean flag shared with AppManagerProvider.
const STORAGE_KEY = 'echobird_easter_egg';

function readEnabled(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) !== 'false';
  } catch {
    return true;
  }
}

interface EasterEggState {
  enabled: boolean;
  setEnabled: (enabled: boolean) => void;
}

export const useEasterEggStore = create<EasterEggState>((set) => ({
  enabled: readEnabled(),
  setEnabled: (enabled) => {
    set({ enabled });
    try {
      localStorage.setItem(STORAGE_KEY, String(enabled));
    } catch {
      // Unavailable storage must not block the current session's choice.
    }
  },
}));
