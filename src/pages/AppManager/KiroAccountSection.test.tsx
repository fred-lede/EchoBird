import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, expect, it, vi } from 'vitest';
import type { AppManagerContextType } from './context';
import type { KiroAccount } from '../../api/tauri';
import { KiroAccountSection } from './KiroAccountSection';

let state: AppManagerContextType;
let renderer: ReactTestRenderer | undefined;
vi.mock('./context', () => ({ useAppManager: () => state }));
vi.mock('../../hooks/useI18n', () => ({ useI18n: () => ({ t: (key: string) => key }) }));
afterEach(() => {
  act(() => renderer?.unmount());
  renderer = undefined;
});

function fixture(edition: 'kirocrew' | 'kirocli', quota: KiroAccount['quota'] = []) {
  const account: KiroAccount = {
    id: 'saved',
    edition,
    name: 'user@example.test',
    active: true,
    plan: 'Pro',
    quota,
  };
  const group = {
    accounts: [account],
    selectedId: account.id,
    busy: false,
    remainingSeconds: 0,
    refreshing: new Set<string>(),
    authorizationFailedIds: new Set<string>(),
    select: vi.fn(),
    add: vi.fn(),
    refresh: vi.fn(),
    remove: vi.fn(),
    cancelLogin: vi.fn(),
  };
  state = { selectedTool: edition, kiroAccounts: group } as unknown as AppManagerContextType;
  return { account, group };
}

it.each(['kirocrew', 'kirocli'] as const)(
  '%s: uses the shared monochrome rows, quota data and separate actions',
  (edition) => {
    const { account, group } = fixture(edition, [
      { label: 'credits', remaining: 78.5, total: 100 },
      { label: 'trial', remaining: null, total: 500 },
    ]);
    act(() => {
      renderer = create(<KiroAccountSection />);
    });
    const html = renderToStaticMarkup(<KiroAccountSection />);
    expect(html).toContain(`/icons/tools/${edition}.svg`);
    expect(html).toContain('78.5');
    expect(html).toContain('—');
    expect(html).not.toContain('workbuddy');
    expect(html).not.toContain('agent.claimDailyCredits');
    expect(html).not.toContain('cursor-pointer');
    const stopPropagation = vi.fn();
    act(() =>
      renderer!.root
        .findByProps({ 'aria-label': `agent.refreshAccount ${account.name}` })
        .props.onClick({ stopPropagation })
    );
    expect(group.refresh).toHaveBeenCalledExactlyOnceWith(account);
    expect(stopPropagation).toHaveBeenCalledTimes(1);
    expect(group.select).not.toHaveBeenCalled();
    act(() =>
      renderer!.root
        .findByProps({ 'aria-label': `btn.delete ${account.name}` })
        .props.onClick({ stopPropagation })
    );
    expect(group.remove).toHaveBeenCalledExactlyOnceWith(account);
    expect(group.select).not.toHaveBeenCalled();
  }
);

it('disables refresh while pending and preserves keyboard selection and unknown quota', () => {
  const { account, group } = fixture('kirocrew');
  group.refreshing.add(account.id);
  act(() => {
    renderer = create(<KiroAccountSection />);
  });
  expect(
    renderer!.root.findByProps({ 'aria-label': `agent.refreshAccount ${account.name}` }).props
      .disabled
  ).toBe(true);
  const row = renderer!.root.findByProps({ role: 'radio' });
  const preventDefault = vi.fn();
  act(() => row.props.onKeyDown({ key: 'Enter', target: row, currentTarget: row, preventDefault }));
  expect(group.select).toHaveBeenCalledExactlyOnceWith(account.id);
  const html = renderToStaticMarkup(<KiroAccountSection />);
  expect(html).toContain('—');
  expect(html).not.toContain('>0<');
});
