import type { LocalTool } from '../api/types';

// Retain decoded images across refreshes and tab remounts, including fallback choices.
const images = new Map<string, Promise<HTMLImageElement | null>>();

function loadImage(src: string): Promise<HTMLImageElement | null> {
  let pending = images.get(src);
  if (!pending) {
    const image = new Image();
    image.src = src;
    pending = image.decode().then(
      () => image,
      () => null
    );
    images.set(src, pending);
  }
  return pending;
}

export function preloadToolIcons(tools: LocalTool[]): Promise<LocalTool[]> {
  return Promise.all(
    tools.map(async (tool) => {
      const sources = [
        ...(['dsh', 'musecode'].includes(tool.id) ? [] : [`./icons/tools/${tool.id}.svg`]),
        `./icons/tools/${tool.id}.png`,
        ...(tool.iconBase64 ? [tool.iconBase64] : []),
      ];
      for (const src of sources) {
        const image = await loadImage(src);
        if (image) return { ...tool, icon: src };
      }
      return { ...tool, icon: '' };
    })
  );
}
