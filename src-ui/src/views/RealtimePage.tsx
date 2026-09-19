import { useCallback, useContext, useEffect, useRef, useState } from "react";
import {
  App as AntdApp,
  Button,
  Select,
  Tooltip,
  Typography,
} from "antd";
import {
  AudioOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  PushpinOutlined,
  ReloadOutlined,
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

/** 状态 → 状态文字（完整映射，不再用三元兜底掩盖 failed/connecting） */
const STATUS_TEXT: Record<RealtimeStateInfo["state"], string> = {
  idle: "未开始",
  connecting: "连接中…",
  listening: "正在监听",
  paused: "已暂停",
  reconnecting: "重连中…",
  stopped: "已停止",
  failed: "出错了",
};

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
  const [lastError, setLastError] = useState<string | null>(null);
  const [, setSessionId] = useState<string | null>(null);
  const [overlayOn, setOverlayOn] = useState(false);

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
      if (e.state === "failed") {
        setLastError(e.error ?? "未知错误");
      } else if (e.state === "listening") {
        setLastError(null);
      }
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
      setLastError(null);
      setSubtitles([]);
      // 乐观置 connecting；listening 由后端 state-change 事件驱动
      setState("connecting");
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

  /** failed/stopped 后的出口：回到设置页重新开始（保留音源与语言选择） */
  const onNewSession = () => {
    setState("idle");
    setLastError(null);
    setSubtitles([]);
    setCurrentPartial("");
  };

  const onToggleOverlay = async () => {
    try {
      setOverlayOn(await backend.toggleRealtimeOverlay());
    } catch (e) {
      message.error(`悬浮字幕切换失败：${e}`);
    }
  };

  const formatTime = (seconds: number): string => {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = Math.floor(seconds % 60);
    return `${h.toString().padStart(2, "0")}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
  };

  // 音源下拉：系统 / 应用 / 麦克风 三组，组内按名称排序（中文按拼音序）
  const collator = new Intl.Collator("zh-Hans-CN", { numeric: true });
  const toOption = (t: CaptureTarget) => ({ value: t.id, label: t.name });
  const sorted = (list: CaptureTarget[]) =>
    [...list].sort((a, b) => collator.compare(a.name, b.name));

  const systemTarget = targets.find((t) => t.id === "system:all");
  const appTargets = sorted(
    targets.filter((t) => t.id.startsWith("system:") && t.id !== "system:all")
  );
  const micTargets = sorted(targets.filter((t) => t.kind === "microphone"));

  const targetOptions = [
    ...(systemTarget
      ? [{ label: "系统", options: [toOption(systemTarget)] }]
      : []),
    ...(appTargets.length
      ? [{ label: "应用", options: appTargets.map(toOption) }]
      : []),
    ...(micTargets.length
      ? [{ label: "麦克风", options: micTargets.map(toOption) }]
      : []),
  ];

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
              <Select
                mode="multiple"
                showSearch
                placeholder="搜索或选择音源"
                value={selectedTargets}
                onChange={setSelectedTargets}
                options={targetOptions}
                optionFilterProp="label"
                maxTagCount="responsive"
                style={{ width: "100%", textAlign: "left" }}
              />
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
              <span className="realtime-status-text">{STATUS_TEXT[state]}</span>
            </div>
            <div className="realtime-actions">
              <Tooltip title={overlayOn ? "收起悬浮字幕" : "悬浮字幕（置顶悬浮字幕条）"}>
                <Button
                  type={overlayOn ? "primary" : "default"}
                  shape="circle"
                  icon={<PushpinOutlined />}
                  onClick={onToggleOverlay}
                  aria-label="悬浮字幕"
                />
              </Tooltip>
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
              {(state === "failed" || state === "stopped") && (
                <Button type="primary" icon={<ReloadOutlined />} onClick={onNewSession}>
                  新建会话
                </Button>
              )}
            </div>
          </div>

          {state === "failed" && lastError && (
            <div className="realtime-error">
              <Typography.Text type="danger" style={{ fontSize: 12.5 }}>
                {lastError}
              </Typography.Text>
            </div>
          )}

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
