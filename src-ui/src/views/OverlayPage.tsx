import { useContext, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { BackendContext } from "../lib/backend";
import { getOverlayOpacity } from "../lib/overlay-style";
import type { SubtitleEntry } from "../lib/types";

interface OverlayLine {
  index: number;
  entry: SubtitleEntry;
}

/** 悬浮字幕窗：歌词条式，最近 3 句原文+译文，末尾跟实时口述行。
 *  窗口高度跟随内容自动伸缩，从根上杜绝溢出裁切。 */
export default function OverlayPage() {
  const backend = useContext(BackendContext);
  const [lines, setLines] = useState<OverlayLine[]>([]);
  const [partial, setPartial] = useState("");
  const [opacity, setOpacity] = useState(getOverlayOpacity);
  const containerRef = useRef<HTMLDivElement>(null);

  // 主窗设置里拖动滑杆 → storage 事件实时同步到悬浮窗
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === "overlayBackgroundOpacity") setOpacity(getOverlayOpacity());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  // 内容变化后自动调整窗口高度（封顶 300px，超出后容器内部滚动）
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const height = Math.min(Math.max(el.offsetHeight, 60), 300);
    getCurrentWindow()
      .setSize(new LogicalSize(600, height))
      .catch(() => {});
    // 滚动到底部，最新内容始终可见
    requestAnimationFrame(() => { el.scrollTop = el.scrollHeight; });
  }, [lines, partial]);

  useEffect(() => {
    let unlistenFinal: (() => void) | null = null;
    let unlistenTranslation: (() => void) | null = null;
    let unlistenPartial: (() => void) | null = null;
    let unlistenState: (() => void) | null = null;

    backend.onSubtitleFinal((e) => {
      setLines((prev) => [...prev.slice(-2), { index: e.entry_index, entry: e.entry }]);
      setPartial("");
    }).then((fn) => { unlistenFinal = fn; });

    backend.onTranslation((e) => {
      setLines((prev) =>
        prev.map((l) =>
          l.index === e.entry_index
            ? { ...l, entry: { ...l.entry, translated: e.translated } }
            : l
        )
      );
    }).then((fn) => { unlistenTranslation = fn; });

    backend.onSubtitlePartial((e) => {
      setPartial(e.source);
    }).then((fn) => { unlistenPartial = fn; });

    // 新会话从 0 重计序号：listening 事件到达时清屏，避免新旧条目按 index 错误合并
    backend.onRealtimeStateChange((e) => {
      if (e.state === "listening") {
        setLines([]);
        setPartial("");
      }
    }).then((fn) => { unlistenState = fn; });

    return () => {
      unlistenFinal?.();
      unlistenTranslation?.();
      unlistenPartial?.();
      unlistenState?.();
    };
  }, [backend]);

  return (
    <div
      ref={containerRef}
      className="overlay-container"
      data-tauri-drag-region
      style={{ background: `rgba(0, 0, 0, ${opacity})` }}
    >
      {lines.map((l) => (
        <div key={l.index} className="overlay-line">
          <div className="overlay-source">{l.entry.source}</div>
          {l.entry.translated && (
            <div className="overlay-translated">{l.entry.translated}</div>
          )}
        </div>
      ))}
      {partial && <div className="overlay-source overlay-partial">{partial}</div>}
    </div>
  );
}
