import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ModelConfig } from '../../api/types';
import { getReasoningLevels, loadReasoningPreferences } from './reasoning';

const model = (modelId: string, changes: Partial<ModelConfig> = {}): ModelConfig => ({
  internalId: 'fixture',
  name: modelId,
  modelId,
  baseUrl: 'https://api.example.test/v1',
  apiKey: 'fixture',
  ...changes,
});

afterEach(() => vi.unstubAllGlobals());

describe('Mother reasoning capability selection', () => {
  it('offers DeepSeek native levels through either supported protocol', () => {
    expect(getReasoningLevels(model('deepseek-flash'))).toEqual(['low', 'high', 'max']);
    expect(
      getReasoningLevels(
        model('deepseek-v4-pro', {
          baseUrl: '',
          anthropicUrl: 'https://api.example.test/anthropic',
        })
      )
    ).toEqual(['low', 'high', 'max']);
  });

  it('uses the selected wire protocol and exact model family', () => {
    expect(getReasoningLevels(model('gpt-5.2-2025-12-11'))).toEqual([
      'low',
      'medium',
      'high',
      'xhigh',
    ]);
    expect(
      getReasoningLevels(
        model('claude-opus-4-6-20260205', { baseUrl: '', anthropicUrl: 'https://api.example.test' })
      )
    ).toEqual(['low', 'medium', 'high', 'max']);
    expect(
      getReasoningLevels(model('claude-opus-4-6', { anthropicUrl: 'https://api.example.test' }))
    ).toEqual([]);
    for (const id of [
      'gpt-5.2-chat-latest',
      'gpt-5.2-pro',
      'gpt-5.20',
      'gpt-5.2-2025-1211',
      'gpt-6.1-sol',
    ]) {
      expect(getReasoningLevels(model(id))).toEqual([]);
    }
  });

  it('keeps missing, unknown, dynamic router, and unsupported gateway capabilities at default', () => {
    expect(getReasoningLevels()).toEqual([]);
    expect(getReasoningLevels(model('custom-model'))).toEqual([]);
    expect(getReasoningLevels(model('gpt-5.2', { scope: 'smartRouter' }))).toEqual([]);
    expect(
      getReasoningLevels(model('gpt-5.2', { baseUrl: 'https://openrouter.ai/api/v1' }))
    ).toEqual([]);
    expect(getReasoningLevels(model('gpt-5.2', { baseUrl: '' }))).toEqual([]);
  });

  it('uses the real provider capabilities when a Messages endpoint maps Claude aliases', () => {
    expect(
      getReasoningLevels(
        model('claude-opus-5', { baseUrl: '', anthropicUrl: 'https://api.deepseek.com/anthropic' })
      )
    ).toEqual(['low', 'high', 'max']);
    expect(
      getReasoningLevels(
        model('claude-opus-5', { baseUrl: '', anthropicUrl: 'https://api.anthropic.com' })
      )
    ).toEqual(['low', 'medium', 'high', 'xhigh', 'max']);
  });

  it('recovers safely from missing, malformed, and stale local preferences', () => {
    for (const stored of [null, '{bad', 'null', '[]', '"high"']) {
      vi.stubGlobal('localStorage', { getItem: () => stored });
      expect(loadReasoningPreferences()).toEqual({});
    }
    vi.stubGlobal('localStorage', {
      getItem: () => '{"model":"high","other":"invalid","object":["high"]}',
    });
    expect(loadReasoningPreferences()).toEqual({ model: 'high' });
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('unavailable');
      },
    });
    expect(loadReasoningPreferences()).toEqual({});
  });
});
