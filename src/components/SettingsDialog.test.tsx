import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SettingsDialog } from './SettingsDialog';
import { useUiScaleStore } from '../stores/uiScaleStore';
import { useEasterEggStore } from '../stores/easterEggStore';

const mocks = vi.hoisted(() => ({ zoom: vi.fn(), save: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '' }));
vi.mock('@tauri-apps/api/webview', () => ({ getCurrentWebview: () => ({ setZoom: mocks.zoom }) }));
vi.mock('@tauri-apps/api/window', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@tauri-apps/api/window')>()),
  currentMonitor: async () => null,
  getCurrentWindow: () => ({}),
}));
vi.mock('@tauri-apps/plugin-autostart', () => ({ isEnabled: async () => false }));
vi.mock('../api/tauri', () => ({ getSettings: async () => ({}), saveSettings: mocks.save }));
vi.mock('../hooks/useI18n', () => ({ useI18n: () => ({ t: (key: string) => key }) }));
vi.mock('../stores/themeStore', () => ({
  useThemeStore: (select: (state: unknown) => unknown) =>
    select({ mode: 'dark', colorTheme: 'oatgray' }),
}));

let view: ReactTestRenderer;
const props = { isOpen: true, onClose: vi.fn(), locale: 'en', onLocaleChange: vi.fn() };
beforeEach(() => {
  vi.clearAllMocks();
  mocks.zoom.mockResolvedValue(undefined);
  vi.stubGlobal('window', { addEventListener: vi.fn(), removeEventListener: vi.fn() });
  vi.stubGlobal('document', { addEventListener: vi.fn(), removeEventListener: vi.fn() });
  vi.stubGlobal('localStorage', { getItem: vi.fn(), setItem: vi.fn() });
  useEasterEggStore.setState({ enabled: true });
  useUiScaleStore.setState({
    preference: 'auto',
    requestedPreference: null,
    scale: 1,
    pending: false,
    failed: false,
  });
});
afterEach(async () => {
  await act(async () => view?.unmount());
  vi.unstubAllGlobals();
});

async function tab(label: string) {
  await act(async () => {
    view.root
      .findAllByType('button')
      .find((button) => button.props.children.includes(label))!
      .props.onClick();
  });
}

function easterEggSwitch() {
  return view.root
    .findByProps({ children: 'settings.easterEgg' })
    .parent!.parent!.findByProps({ role: 'switch' });
}

function EasterEggObserver() {
  const enabled = useEasterEggStore((state) => state.enabled);
  return <span data-easter-egg-state>{String(enabled)}</span>;
}

it('updates the shared Easter Egg setting and other consumers immediately from the existing switch', async () => {
  await act(async () => {
    view = create(
      <>
        <SettingsDialog {...props} />
        <EasterEggObserver />
      </>
    );
  });
  expect(easterEggSwitch().props['aria-checked']).toBe(true);
  await act(async () => {
    easterEggSwitch().props.onClick();
  });
  expect(easterEggSwitch().props['aria-checked']).toBe(false);
  expect(view.root.findByProps({ 'data-easter-egg-state': true }).props.children).toBe('false');
  expect(localStorage.setItem).toHaveBeenCalledExactlyOnceWith('echobird_easter_egg', 'false');
  expect(mocks.save).not.toHaveBeenCalled();
});

it('reflects a shared Easter Egg change while settings is already open', async () => {
  await act(async () => {
    view = create(<SettingsDialog {...props} />);
  });
  await act(async () => {
    useEasterEggStore.getState().setEnabled(false);
  });
  expect(easterEggSwitch().props['aria-checked']).toBe(false);
});

it.each([true, false])(
  'preserves the shared %s Easter Egg choice through tabs and reopening without storage writes',
  async (enabled) => {
    useEasterEggStore.setState({ enabled });
    await act(async () => {
      view = create(<SettingsDialog {...props} />);
    });
    expect(easterEggSwitch().props['aria-checked']).toBe(enabled);
    await tab('settings.appearance');
    await tab('settings.general');
    await act(async () => {
      view.update(<SettingsDialog {...props} isOpen={false} />);
    });
    await act(async () => {
      view.update(<SettingsDialog {...props} />);
    });
    expect(easterEggSwitch().props['aria-checked']).toBe(enabled);
    expect(localStorage.getItem).not.toHaveBeenCalled();
    expect(localStorage.setItem).not.toHaveBeenCalled();
    expect(mocks.save).not.toHaveBeenCalled();
  }
);

it('keeps Easter Egg disabled in memory and on reopening when storage is unavailable', async () => {
  vi.mocked(localStorage.getItem).mockImplementation(() => {
    throw new Error('storage unavailable');
  });
  vi.mocked(localStorage.setItem).mockImplementation(() => {
    throw new Error('storage unavailable');
  });
  await act(async () => {
    view = create(<SettingsDialog {...props} />);
  });
  await act(async () => {
    easterEggSwitch().props.onClick();
  });
  expect(useEasterEggStore.getState().enabled).toBe(false);
  expect(easterEggSwitch().props['aria-checked']).toBe(false);
  await act(async () => {
    view.update(<SettingsDialog {...props} isOpen={false} />);
  });
  await act(async () => {
    view.update(<SettingsDialog {...props} />);
  });
  expect(easterEggSwitch().props['aria-checked']).toBe(false);
  expect(localStorage.getItem).not.toHaveBeenCalled();
  expect(localStorage.setItem).toHaveBeenCalledExactlyOnceWith('echobird_easter_egg', 'false');
  expect(mocks.save).not.toHaveBeenCalled();
});

it('only changes scale on explicit selection, not settings entry, tab changes or reopening', async () => {
  await act(async () => {
    view = create(<SettingsDialog {...props} />);
  });
  await tab('settings.appearance');
  await tab('settings.general');
  await tab('settings.appearance');
  await act(async () => {
    view.update(<SettingsDialog {...props} isOpen={false} />);
  });
  await act(async () => {
    view.update(<SettingsDialog {...props} />);
  });
  expect(mocks.zoom).not.toHaveBeenCalled();
  expect(mocks.save).not.toHaveBeenCalled();
  expect(localStorage.setItem).not.toHaveBeenCalled();

  const slider = view.root.findByProps({ id: 'ui-scale' });
  expect(slider.props).toMatchObject({ type: 'range', min: 70, max: 150, step: 'any', value: 100 });
  await act(async () => {
    slider.props.onChange({ target: { value: '110' } });
  });
  expect(mocks.zoom).toHaveBeenCalledExactlyOnceWith(1.1);
  expect(localStorage.setItem).toHaveBeenCalledExactlyOnceWith('echobird-ui-scale', '110');
});

it('keeps the slider responsive while applying and shows failure without a blocking dialog', async () => {
  await act(async () => {
    view = create(<SettingsDialog {...props} />);
  });
  await tab('settings.appearance');
  await act(async () => {
    useUiScaleStore.setState({ pending: true, requestedPreference: 70 });
  });
  expect(view.root.findByProps({ id: 'ui-scale' }).props.disabled).toBeUndefined();
  expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(70);
  await act(async () => {
    useUiScaleStore.setState({ pending: false, failed: true, requestedPreference: null });
  });
  expect(view.root.findByProps({ role: 'alert' }).props.children).toBe('settings.uiScaleError');
  expect(view.root.findByProps({ id: 'ui-scale' }).props.disabled).toBeUndefined();
});

it('keeps the original drag coordinates as the window and UI resize and can return to automatic', async () => {
  await act(async () => {
    view = create(<SettingsDialog {...props} />);
  });
  await tab('settings.appearance');
  const target = {
    focus: vi.fn(),
    setPointerCapture: vi.fn(),
    getBoundingClientRect: () => ({ left: 100, width: 414 }),
  };
  await act(async () => {
    view.root.findByProps({ id: 'ui-scale' }).props.onPointerDown({
      button: 0,
      preventDefault: vi.fn(),
      currentTarget: target,
      pointerId: 1,
      clientX: 257,
      screenX: 1000,
    });
    // The new client coordinate deliberately disagrees after zoom/recentering.
    view.root.findByProps({ id: 'ui-scale' }).props.onPointerMove({ screenX: 1250, clientX: 10 });
    view.root.findByProps({ id: 'ui-scale' }).props.onPointerUp();
  });
  expect(target.setPointerCapture).toHaveBeenCalledWith(1);
  expect(useUiScaleStore.getState()).toMatchObject({ preference: 150, scale: 1.5 });
  expect(localStorage.setItem).toHaveBeenLastCalledWith('echobird-ui-scale', '150');
  await act(async () => {
    view.root.findByProps({ id: 'ui-scale' }).props.onPointerMove({ screenX: 0 });
  });
  expect(useUiScaleStore.getState().preference).toBe(150);
  await act(async () => {
    view.root
      .findAllByType('button')
      .find((button) => button.props.children === 'settings.uiScaleAuto')!
      .props.onClick();
  });
  expect(useUiScaleStore.getState().preference).toBe('auto');
  expect(localStorage.setItem).toHaveBeenLastCalledWith('echobird-ui-scale', 'auto');
});

async function startScaleDrag(percent = 100) {
  await act(async () => {
    view = create(<SettingsDialog {...props} />);
  });
  await tab('settings.appearance');
  await act(async () => {
    view.root.findByProps({ id: 'ui-scale' }).props.onPointerDown({
      button: 0,
      preventDefault: vi.fn(),
      pointerId: 1,
      clientX: 107 + (percent - 70) * 5,
      screenX: 1000,
      currentTarget: {
        focus: vi.fn(),
        setPointerCapture: vi.fn(),
        getBoundingClientRect: () => ({ left: 100, width: 414 }),
      },
    });
  });
}

it.each(['close', 'tab'])(
  'clears an interrupted scale drag on %s before keyboard input',
  async (exit) => {
    await startScaleDrag();
    await act(async () => {
      view.root.findByProps({ id: 'ui-scale' }).props.onPointerMove({ screenX: 1035 });
    });
    vi.mocked(localStorage.setItem).mockClear();
    if (exit === 'close') {
      await act(async () => {
        view.update(<SettingsDialog {...props} isOpen={false} />);
      });
      await act(async () => {
        view.update(<SettingsDialog {...props} />);
      });
    } else {
      await tab('settings.general');
      await tab('settings.appearance');
    }
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(100);
    expect(localStorage.setItem).not.toHaveBeenCalled();
    await act(async () => {
      view.root.findByProps({ id: 'ui-scale' }).props.onChange({ target: { value: '110' } });
    });
    expect(useUiScaleStore.getState().preference).toBe(110);
    expect(localStorage.setItem).toHaveBeenLastCalledWith('echobird-ui-scale', '110');
  }
);

it('shows nine non-interactive ticks at every ten percent', async () => {
  await startScaleDrag();
  const ticks = view.root.findByProps({ 'data-scale-ticks': true });
  expect(ticks.props['aria-hidden']).toBe(true);
  expect(ticks.findAllByType('span').map((tick) => tick.props['data-scale-tick'])).toEqual([
    70, 80, 90, 100, 110, 120, 130, 140, 150,
  ]);
});

it.each([
  [72, 70],
  [78, 80],
  [98, 100],
  [102, 100],
  [108, 110],
  [148, 150],
  [74, 70],
  [75, 80],
  [77, 80],
  [125, 130],
])(
  'attracts a pointer press at %s percent to the nearby %s percent tick',
  async (pointer, expected) => {
    await startScaleDrag(pointer);
    expect(useUiScaleStore.getState().preference).toBe(expected);
  }
);

it.each([
  [97, 100],
  [103, 100],
  [106, 100],
  [114, 110],
  [86, 90],
  [160, 150],
  [60, 70],
])(
  'moves the thumb to %s but applies only the last crossed %s percent tick',
  async (pointer, expected) => {
    await startScaleDrag();
    await act(async () => {
      view.root
        .findByProps({ id: 'ui-scale' })
        .props.onPointerMove({ screenX: 1000 + (pointer - 100) * 5 });
    });
    expect(useUiScaleStore.getState().preference).toBe(expected);
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(
      Math.max(70, Math.min(150, pointer))
    );
    expect(view.root.findByProps({ id: 'ui-scale' }).props['aria-valuetext']).toBe(`${expected}%`);
    await act(async () => {
      view.root.findByProps({ id: 'ui-scale' }).props.onPointerUp();
    });
    expect(useUiScaleStore.getState().preference).toBe(expected);
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(expected);
    expect(localStorage.setItem).toHaveBeenLastCalledWith('echobird-ui-scale', String(expected));
  }
);

it('waits for the next tick in either direction while the thumb moves freely', async () => {
  await startScaleDrag();
  for (const [pointer, expected] of [
    [101, 100],
    [109.9, 100],
    [110, 110],
    [107, 110],
    [100.1, 110],
    [100, 100],
    [99, 100],
    [90, 90],
  ]) {
    mocks.zoom.mockClear();
    vi.mocked(localStorage.setItem).mockClear();
    const previous = useUiScaleStore.getState().preference;
    await act(async () => {
      view.root
        .findByProps({ id: 'ui-scale' })
        .props.onPointerMove({ screenX: 1000 + (pointer - 100) * 5 });
    });
    expect(useUiScaleStore.getState().preference).toBe(expected);
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBeCloseTo(pointer);
    if (expected === previous) {
      expect(mocks.zoom).not.toHaveBeenCalled();
      expect(localStorage.setItem).not.toHaveBeenCalled();
    } else {
      expect(mocks.zoom).toHaveBeenCalledExactlyOnceWith(expected / 100);
      expect(localStorage.setItem).toHaveBeenCalledExactlyOnceWith(
        'echobird-ui-scale',
        String(expected)
      );
    }
  }
});

it.each(['onPointerUp', 'onPointerCancel', 'onLostPointerCapture'])(
  'returns the thumb to the reached tick on %s without another scale change',
  async (finish) => {
    await startScaleDrag();
    await act(async () => {
      view.root.findByProps({ id: 'ui-scale' }).props.onPointerMove({ screenX: 1085 });
    });
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(117);
    expect(useUiScaleStore.getState().preference).toBe(110);
    mocks.zoom.mockClear();
    vi.mocked(localStorage.setItem).mockClear();
    await act(async () => {
      view.root.findByProps({ id: 'ui-scale' }).props[finish]();
      view.root.findByProps({ id: 'ui-scale' }).props.onPointerMove({ screenX: 1250 });
    });
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(110);
    expect(mocks.zoom).not.toHaveBeenCalled();
    expect(localStorage.setItem).not.toHaveBeenCalled();
  }
);

it('keeps keyboard changes at ten percent with a continuous pointer range', async () => {
  await startScaleDrag();
  await act(async () => {
    view.root.findByProps({ id: 'ui-scale' }).props.onPointerUp();
  });
  for (const [key, expected] of [
    ['ArrowRight', 110],
    ['ArrowUp', 120],
    ['ArrowLeft', 110],
    ['ArrowDown', 100],
    ['End', 150],
    ['ArrowRight', 150],
    ['Home', 70],
    ['ArrowLeft', 70],
  ] as const) {
    const preventDefault = vi.fn();
    await act(async () => {
      view.root.findByProps({ id: 'ui-scale' }).props.onKeyDown({ key, preventDefault });
    });
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(useUiScaleStore.getState().preference).toBe(expected);
    expect(view.root.findByProps({ id: 'ui-scale' }).props.value).toBe(expected);
  }
});
