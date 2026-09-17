import { useContext, useEffect, useState } from "react";
import { BackendContext } from "../lib/backend";
import type { SubtitleEntry } from "../lib/types";

export default function OverlayPage() {
  const backend = useContext(BackendContext);
  const [recent, setRecent] = useState<SubtitleEntry[]>([]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;

    backend.onSubtitleFinal((e) => {
      setRecent((prev) => [...prev.slice(-2), e.entry]); // 只保留最近 3 条
    }).then((fn) => { unlisten = fn; });

    return () => { unlisten?.(); };
  }, [backend]);

  return (
    <div className="overlay-container" data-tauri-drag-region>
      {recent.map((entry, i) => (
        <div key={i} className="overlay-line">
          <div className="overlay-source">{entry.source}</div>
          {entry.translated && (
            <div className="overlay-translated">{entry.translated}</div>
          )}
        </div>
      ))}
    </div>
  );
}
