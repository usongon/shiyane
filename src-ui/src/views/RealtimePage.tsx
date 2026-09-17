import { useCallback, useContext, useEffect, useRef, useState } from "react";
import {
  App as AntdApp,
  Button,
  Checkbox,
  Select,
  Typography,
} from "antd";
import {
  AudioOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  StopOutlined,
} from "@ant-design/icons";
import { BackendContext } from "../lib/backend";
import type {
  CaptureTarget,
  RealtimeStateInfo,
} from "../lib/types";

const LANGUAGES = [
  { value: "auto", label: "自动识别" },
  { value: "zh", label: "中文" },
  { value: "en", label: "英文" },
  { value: "ja", label: "日文" },
  { value: "ko", label: "韩文" },
];

interface SubtitleLine {
  index: number;
  timestamp: string;
  source: string;
  translated: string;
  isPartial: boolean;
}

export default function RealtimePage({ active }: { active: boolean }) {
  const backend = useContext(BackendContext);
  const { message } = AntdApp.useApp();

  const [state, setState] = useState<RealtimeStateInfo["state"]>("idle");
  const [targets, setTargets] = useState<CaptureTarget[]>([]);
  const [selectedTargets, setSelectedTargets] = useState<string[]>([]);
  const [language, setLanguage] = useState("auto");
  const [subtitles, setSubtitles] = useState<SubtitleLine[]>([]);
  const [currentPartial, setCurrentPartial] = useState<string>("");
  const [, setSessionId] = useState<string | null>(null);

  const listRef = useRef<HTMLDivElement>(null);
  const autoScrollRef = useRef(true);

  // 加载可用音源
  useEffect(() => {
    if (!active) return;
    backend
      .listCaptureTargets()
      .then(setTargets)
      .catch((e) => message.error(`获取音源列表失败：${e}`));
  }, [active, backend, message]);

  // 订阅事件
  useEffect(() => {
    if (!active) return;

    let unlistenPartial: (() => void) | null = null;
    let unlistenFinal: (() => void) | null = null;
    let unlistenTranslation: (() => void) | null = null;
    let unlistenState: (() => void) | null = null;

    backend.onSubtitlePartial((e) => {
      setCurrentPartial(e.source);
    }).then((fn) => { unlistenPartial = fn; });

    backend.onSubtitleFinal((e) => {
      const line: SubtitleLine = {
        index: e.entry_index,
        timestamp: formatTime(e.entry.content_start),
        source: e.entry.source,
        translated: "",
        isPartial: false,
      };
      setSubtitles((prev) => [...prev, line]);
      setCurrentPartial("");
    }).then((fn) => { unlistenFinal = fn; });

    backend.onTranslation((e) => {
      setSubtitles((prev) =>
        prev.map((s) =>
          s.index === e.entry_index ? { ...s, translated: e.translated } : s
        )
      );
    }).then((fn) => { unlistenTranslation = fn; });

    backend.onRealtimeStateChange((e) => {
      setState(e.state);
      if (e.session_id) setSessionId(e.session_id);
    }).then((fn) => { unlistenState = fn; });

    return () => {
      unlistenPartial?.();
      unlistenFinal?.();
      unlistenTranslation?.();
      unlistenState?.();
    };
  }, [active, backend]);

  // 自动滚动
  useEffect(() => {
    if (autoScrollRef.current && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [subtitles, currentPartial]);

  const onScroll = useCallback(() => {
    if (!listRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = listRef.current;
    autoScrollRef.current = scrollHeight - scrollTop - clientHeight < 50;
  }, []);

  const onStart = async () => {
    if (selectedTargets.length === 0) {
      message.warning("请选择至少一个音源");
      return;
    }
    try {
      const id = await backend.startRealtime(language, selectedTargets);
      setSessionId(id);
      setState("listening");
      setSubtitles([]);
    } catch (e) {
      message.error(`启动失败：${e}`);
    }
  };

  const onPause = async () => {
    try {
      await backend.pauseRealtime();
      setState("paused");
    } catch (e) {
      message.error(`暂停失败：${e}`);
    }
  };

  const onResume = async () => {
    try {
      await backend.resumeRealtime();
      setState("listening");
    } catch (e) {
      message.error(`恢复失败：${e}`);
    }
  };

  const onStop = async () => {
    try {
      await backend.stopRealtime();
      setState("stopped");
    } catch (e) {
      message.error(`停止失败：${e}`);
    }
  };

  const formatTime = (seconds: number): string => {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = Math.floor(seconds % 60);
    return `${h.toString().padStart(2, "0")}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
  };

  const isListening = state === "listening";
  const isPaused = state === "paused";
  const isIdle = state === "idle";

  return (
    <div className="view" aria-hidden={!active}>
      {isIdle ? (
        <div className="realtime-setup">
          <div className="coming-icon">
            <AudioOutlined style={{ fontSize: 32, color: "#fff" }} />
          </div>
          <Typography.Text strong style={{ fontSize: 15 }}>
            实时字幕
          </Typography.Text>
          <Typography.Text type="secondary" style={{ fontSize: 12.5 }}>
            边说话边出字幕 · 基于百炼实时识别
          </Typography.Text>

          <div className="realtime-form">
            <div className="realtime-form-section">
              <div className="realtime-form-label">音源选择</div>
              <Checkbox.Group
                value={selectedTargets}
                onChange={(vals) => setSelectedTargets(vals as string[])}
              >
                {targets.map((t) => (
                  <div key={t.id} className="realtime-target-row">
                    <Checkbox value={t.id}>
                      {t.name}
                      <span className="realtime-target-kind">
                        {t.kind === "system_audio" ? "系统音频" : "麦克风"}
                      </span>
                    </Checkbox>
                  </div>
                ))}
              </Checkbox.Group>
            </div>

            <div className="realtime-form-section">
              <div className="realtime-form-label">源语言</div>
              <Select
                value={language}
                onChange={setLanguage}
                options={LANGUAGES}
                style={{ width: 140 }}
              />
            </div>

            <Button
              type="primary"
              icon={<PlayCircleOutlined />}
              onClick={onStart}
              disabled={selectedTargets.length === 0}
              block
            >
              开始监听
            </Button>
          </div>
        </div>
      ) : (
        <div className="realtime-session">
          <div className="realtime-toolbar">
            <div className="realtime-status">
              <span className={`realtime-status-dot ${state}`} />
              <span className="realtime-status-text">
                {isListening ? "正在监听" : isPaused ? "已暂停" : "已停止"}
              </span>
            </div>
            <div className="realtime-actions">
              {isListening && (
                <Button icon={<PauseCircleOutlined />} onClick={onPause}>
                  暂停
                </Button>
              )}
              {isPaused && (
                <Button icon={<PlayCircleOutlined />} onClick={onResume}>
                  恢复
                </Button>
              )}
              {(isListening || isPaused) && (
                <Button danger icon={<StopOutlined />} onClick={onStop}>
                  停止
                </Button>
              )}
            </div>
          </div>

          <div className="realtime-subtitle-list" ref={listRef} onScroll={onScroll}>
            {subtitles.map((line) => (
              <div key={line.index} className="realtime-subtitle-line">
                <div className="realtime-subtitle-timestamp mono">{line.timestamp}</div>
                <div className="realtime-subtitle-source">{line.source}</div>
                {line.translated && (
                  <div className="realtime-subtitle-translated">{line.translated}</div>
                )}
              </div>
            ))}
            {currentPartial && (
              <div className="realtime-subtitle-line partial">
                <div className="realtime-subtitle-timestamp mono">--:--:--</div>
                <div className="realtime-subtitle-source">{currentPartial}</div>
                <div className="realtime-subtitle-translated">翻译中...</div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
