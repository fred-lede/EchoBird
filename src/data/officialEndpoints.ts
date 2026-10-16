// Per-tool "official" endpoint registry. The Restore button in App Manager
// uses this to revert a tool from a third-party / proxy URL back to the
// vendor's canonical address.
//
// Entries should match the shape of an ApplyModelInput so the existing
// applyModelToTool command can write them straight to the tool's config.

export interface OfficialEndpoint {
  /** Display name shown in UI / toasts */
  name: string;
  /** OpenAI-protocol base URL (or "" if N/A) */
  baseUrl: string;
  /** Anthropic-protocol base URL (optional) */
  anthropicUrl?: string;
  /** Which protocol to write into the tool's config */
  protocol: 'openai' | 'anthropic';
  /**
   * Fallback model id used only when the tool has no model configured yet.
   * Restore preserves the user's existing model where possible — this is
   * the seed for fresh installs, not a forced override.
   */
  modelId?: string;
}

/** Sentinel internalId used to mark "official endpoint" as the pending selection. */
export const officialModelSentinel = (toolId: string) => `__official__${toolId}`;
export const isOfficialModelSentinel = (id: string | null | undefined) =>
  typeof id === 'string' && id.startsWith('__official__');

export const OFFICIAL_ENDPOINTS: Record<string, OfficialEndpoint> = {
  claudecode: {
    name: 'Anthropic Official',
    baseUrl: 'https://api.anthropic.com',
    anthropicUrl: 'https://api.anthropic.com',
    protocol: 'anthropic',
    modelId: 'claude-sonnet-4-5',
  },
  claudedesktop: {
    // Restore for Claude Desktop flips deploymentMode back to '1p' and
    // deletes the 3P profile / relay; the URL fields here are display-only
    // (Desktop's official mode uses Anthropic OAuth, not an API key).
    name: 'Anthropic Official',
    baseUrl: 'https://api.anthropic.com',
    anthropicUrl: 'https://api.anthropic.com',
    protocol: 'anthropic',
    modelId: 'claude-sonnet-4-5',
  },
  claudescience: {
    name: 'Anthropic Official',
    baseUrl: 'https://api.anthropic.com',
    anthropicUrl: 'https://api.anthropic.com',
    protocol: 'anthropic',
    modelId: 'claude-sonnet-4-5',
  },
  codex: {
    name: 'OpenAI Official',
    baseUrl: 'https://api.openai.com/v1',
    protocol: 'openai',
    modelId: 'gpt-4o',
  },
  chatgptdesktop: {
    name: 'OpenAI Official',
    baseUrl: 'https://api.openai.com/v1',
    protocol: 'openai',
    modelId: 'gpt-4o',
  },
  grok: {
    name: 'xAI Official',
    baseUrl: 'https://api.x.ai/v1',
    protocol: 'openai',
    modelId: 'grok-4.5',
  },
  mimocode: {
    // Restore removes our provider block; MiMo Code then falls back to the
    // user's native /connect credentials for the Xiaomi MiMo Platform.
    name: 'Xiaomi MiMo Official',
    baseUrl: 'https://api.xiaomimimo.com/v1',
    protocol: 'openai',
    modelId: 'mimo-v2.6-pro',
  },
  // Kimi Code: NO official-endpoint card. Unlike Xiaomi's single MiMo
  // platform, Moonshot runs TWO separate platforms with independent
  // accounts/keys — api.moonshot.ai (global, platform.kimi.ai) and
  // api.moonshot.cn (China, platform.kimi.com) — so picking one as "the
  // official" would misrepresent it as canonical (the .ai entry I first
  // added was wrong for China users). Both are already listed in the model
  // directory as distinct entries; users pick their platform there.
  // Restore-to-official is hidden for kimi, matching openclaw/hermes/opencode.
  // OpenCode is a third-party tool, not OpenAI official
  // Community open-source tools (openclaw, hermes, opencode) have no
  // canonical vendor URL — restore is hidden.
};

export function getOfficialEndpoint(toolId: string): OfficialEndpoint | undefined {
  return OFFICIAL_ENDPOINTS[toolId];
}
