/**
 * 悬浮字幕窗的外观偏好：纯界面设置，存 localStorage（同一 origin 下主窗/
 * 悬浮窗共享），写入方与悬浮窗之间通过 storage 事件实时同步。
 */
const KEY = "overlayBackgroundOpacity";

export function getOverlayOpacity(): number {
  const raw = localStorage.getItem(KEY);
  const v = raw === null ? NaN : Number(raw);
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0.6;
}

export function setOverlayOpacity(v: number): void {
  localStorage.setItem(KEY, String(Math.min(1, Math.max(0, v))));
}
