import { useContext, useEffect, useState } from "react";
import { BackendContext } from "../lib/backend";
import type { SubtitleEntry } from "../lib/types";

interface OverlayLine {
  index: number;
  entry: SubtitleEntry;
}

/** 悬浮字幕窗：歌词条式，最近几句原文+译文，末尾跟实时口述行 */
export default function OverlayPage() {
  const backend = useContext(BackendContext);
  const [lines, setLines] = useState<OverlayLine[]>([]);
  const [partial, setPartial] = useState("");

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
    <div className="overlay-container" data-tauri-drag-region>
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
