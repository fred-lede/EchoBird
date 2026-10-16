import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { beforeAll, describe, expect, it, vi } from 'vitest';

let Sidebar: typeof import('./Sidebar').Sidebar;

beforeAll(async () => {
  vi.stubGlobal('__APP_EDITION__', 'full');
  ({ Sidebar } = await import('./Sidebar'));
});

const baseProps = {
  activePage: 'freeModels' as const,
  onPageChange: vi.fn(),
  smartRouterEnabled: false,
  smartRouterTogglePending: false,
  onSmartRouterChange: vi.fn(),
};

describe('Sidebar', () => {
  it('places AI news immediately above My AI Career', () => {
    const markup = renderToStaticMarkup(<Sidebar {...baseProps} hasSmartRouterModels={false} />);
    expect(markup.indexOf('nav.aiNews')).toBeGreaterThan(-1);
    expect(markup.indexOf('nav.aiNews')).toBeLessThan(markup.indexOf('nav.aiCareer'));
  });

  it('hides the smart router switch when no model is connected', () => {
    const markup = renderToStaticMarkup(<Sidebar {...baseProps} hasSmartRouterModels={false} />);

    expect(markup).not.toContain('role="switch"');
  });

  it('shows the smart router switch when a model is connected', () => {
    const markup = renderToStaticMarkup(<Sidebar {...baseProps} hasSmartRouterModels />);

    expect(markup).toContain('role="switch"');
  });
});
