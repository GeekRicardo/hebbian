export const MIN_PREVIEW_ZOOM = 0.25;
export const MAX_PREVIEW_ZOOM = 4;
export const PREVIEW_ZOOM_STEP = 0.1;

export function nextPreviewZoom(current: number, wheelDeltaY: number) {
  if (wheelDeltaY === 0) return clampPreviewZoom(current);
  const direction = wheelDeltaY < 0 ? 1 : -1;
  return clampPreviewZoom(current + direction * PREVIEW_ZOOM_STEP);
}

function clampPreviewZoom(value: number) {
  return roundZoom(Math.min(MAX_PREVIEW_ZOOM, Math.max(MIN_PREVIEW_ZOOM, value)));
}

function roundZoom(value: number) {
  return Math.round(value * 100) / 100;
}
