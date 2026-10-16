import profiles from '../../data/motherReasoning.json';
import type { ModelConfig, ReasoningEffort } from '../../api/types';

export function getReasoningLevels(model?: ModelConfig): ReasoningEffort[] {
  if (!model || model.scope === 'smartRouter') return [];
  const protocol = !model.baseUrl && model.anthropicUrl ? 'anthropic' : 'openai';
  let id = (model.modelId || model.name).trim().toLowerCase();
  // Gateway reasoning has a separate wire format and history representation.
  try {
    const host = new URL(model.baseUrl || model.anthropicUrl || '').hostname;
    if (host === 'openrouter.ai') return [];
    if (host === 'api.deepseek.com' && protocol === 'anthropic' && id.startsWith('claude-')) {
      id = id.startsWith('claude-opus-') ? 'deepseek-v4-pro' : 'deepseek-flash';
    }
  } catch {
    return [];
  }
  return (profiles.find(
    (profile) =>
      profile.protocols.includes(protocol) &&
      profile.models.some(
        (name) =>
          id === name ||
          new RegExp(`^${name.replace(/\./g, '\\.')}-(?:\\d{8}|\\d{4}-\\d{2}-\\d{2})$`).test(id)
      )
  )?.levels ?? []) as ReasoningEffort[];
}

export const REASONING_STORAGE_KEY = 'echobird_mother_reasoning';

export function loadReasoningPreferences(): Record<string, ReasoningEffort> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(REASONING_STORAGE_KEY) || '{}');
    if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
    return Object.fromEntries(
      Object.entries(value).filter(
        ([, effort]) =>
          typeof effort === 'string' && ['low', 'medium', 'high', 'xhigh', 'max'].includes(effort)
      )
    );
  } catch {
    return {};
  }
}
