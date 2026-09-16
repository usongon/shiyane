import { useCallback, useContext, useEffect, useRef, useState } from "react";
import {
  App as AntdApp,
  Button,
  Progress,
  Select,
  Steps,
  Tooltip,
  Typography,
  message as staticMessage,
  theme as antdTheme,
} from "antd";
import {
  InboxOutlined,
  VideoCameraOutlined,
  PlayCircleOutlined,
  DownloadOutlined,
  ReloadOutlined,
  ArrowLeftOutlined,
  PauseCircleOutlined,
  StopOutlined,
  DeleteOutlined,
} from "@ant-design/icons";
import { BackendContext } from "../lib/backend";
import { basename } from "../lib/types";
import type { PipelineStateName, ProgressInfo, RecentTask, TaskStatus } from "../lib/types";
import { PRIMARY } from "../theme";

const LANGUAGES = [
  { value: "auto", label: "自动" },
  { value: "zh", label: "中文" },
  { value: "en", label: "英文" },
  { value: "ja", label: "日文" },
  { value: "ko", label: "韩文" },
];

const STATUS_LABEL: Record<PipelineStateName, string> = {
  idle: "准备中",
  processing: "处理中",
  paused: "已暂停",
  completed: "已完成",
  exported: "已导出",
  failed: "失败",
};

interface Task {
  id: string;
  fileName: string;
  progress: ProgressInfo;
}

export default function FilePage({ active }: { active: boolean }) {
  const backend = useContext(BackendContext);
  const { message, modal } = AntdApp.useApp();
  const { token } = antdTheme.useToken();

  const [file, setFile] = useState<{ path: string; name: string } | null>(null);
  const [language, setLanguage] = useState("auto");
  const [dragOver, setDragOver] = useState(false);
  const [starting, setStarting] = useState(false);
  const [pausing, setPausing] = useState(false);
  const [task, setTask] = useState<Task | null>(null);
  const [fileStatus, setFileStatus] = useState<TaskStatus | null>(null);
  const [exporting, setExporting] = useState<"srt" | "vtt" | null>(null);
  const [recentTasks, setRecentTasks] = useState<RecentTask[]>([]);

  const pollRef = useRef<number | null>(null);
  const taskRunningRef = useRef(false);

  const taskRunning =
    task !== null && (task.progress.state === "processing" || task.progress.state === "idle");
  taskRunningRef.current = taskRunning;

  const stopPolling = useCallback(() => {
    if (pollRef.current !== null) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  const refreshFileStatus = useCallback(async () => {
    if (!file) return;
    try {
      setFileStatus(await backend.getTaskStatus(file.path));
    } catch {
      /* 保持现状 */
    }
  }, [backend, file]);

  const startPolling = useCallback(() => {
    stopPolling();
    pollRef.current = window.setInterval(async () => {
      try {
        const info = await backend.getProcessingProgress();
        setTask((t) => (t ? { ...t, progress: info } : t));
        if (info.state !== "processing" && info.state !== "idle") {
          stopPolling();
          // 终态后同步可续传状态：头部按钮/清除进度都以 fileStatus 为准，
          // 不刷新会让已完成任务旁边残留「清除进度」
          refreshFileStatus();
        }
      } catch (e) {
        console.error("进度查询失败:", e);
      }
    }, 1000);
  }, [backend, stopPolling, refreshFileStatus]);

  useEffect(() => stopPolling, [stopPolling]);

  useEffect(() => {
    if (!active || file) return;
    backend
      .listRecentTasks()
      .then(setRecentTasks)
      .catch(() => setRecentTasks([]));
  }, [active, file, backend]);

  const selectFile = useCallback(
    (path: string) => {
      setFile({ path, name: basename(path) });
      setTask(null);
      setFileStatus(null);
      backend
        .getTaskStatus(path)
        .then((status) => {
          setFileStatus(status);
          // 可续传/已完成任务自动选回 checkpoint 记录的源语言——
          // 语言不符会触发全量重跑而不是续传
          if (status.state !== "fresh" && status.source_language) {
            setLanguage(status.source_language);
          }
        })
        .catch(() => setFileStatus(null));
    },
    [backend],
  );

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    backend
      .onDragEvent({
        onEnter: () => setDragOver(true),
        onLeave: () => setDragOver(false),
        onDrop: (paths) => {
          setDragOver(false);
          const p = paths[0];
          if (!p) return;
          if (taskRunningRef.current) {
            // 拖拽回调在 React 事件体系之外，App.useApp 的 message 在此上下文不渲染，需走静态 API
            staticMessage.warning("任务处理中，请等待完成后再更换文件");
            return;
          }
          selectFile(p);
        },
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("拖拽监听注册失败:", e));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [backend, selectFile]);

  const onPickFile = async () => {
    try {
      const path = await backend.pickVideoFile();
      if (path) selectFile(path);
    } catch (e) {
      message.error(`打开文件对话框失败：${e}`);
    }
  };

  const onBack = () => {
    if (taskRunning) return;
    setFile(null);
    setFileStatus(null);
    setTask(null);
  };

  const doStart = async () => {
    if (!file) return;
    setStarting(true);
    try {
      const id = await backend.startFileProcessing(file.path, language);
      setTask({
        id,
        fileName: file.name,
        progress: {
          state: "idle",
          progress: fileStatus?.state === "translating" ? fileStatus.percent : 0,
          error: null,
          phase: fileStatus?.state === "translating" ? "translating" : "extracting",
        },
      });
      startPolling();
    } catch (e) {
      message.error(`启动处理失败：${e}`);
    } finally {
      setStarting(false);
    }
  };

  const onStart = () => {
    if (!file || taskRunning) return;
    // 可续传任务的源语言被改过 → 续传失效，弹窗确认后再全量重跑
    if (langChanged && fileStatus) {
      const langLabel = (v: string) => LANGUAGES.find((l) => l.value === v)?.label ?? v;
      modal.confirm({
        title: "源语言已改变",
        content: `该任务已有 ${Math.round(fileStatus.percent * 100)}% 进度（源语言：${langLabel(
          fileStatus.source_language!,
        )}）。当前选择的源语言是「${langLabel(language)}」，语言不同将放弃已有进度全量重跑（含重新转写与上传）。`,
        okText: "放弃进度，全量重跑",
        okButtonProps: { danger: true },
        cancelText: "取消",
        onOk: doStart,
      });
      return;
    }
    doStart();
  };

  const refreshAfterPause = async () => {
    try {
      const info = await backend.getProcessingProgress();
      setTask((t) => (t ? { ...t, progress: info } : t));
    } catch {
      /* 下次轮询兜底 */
    }
    // 刷新可续传状态：头部主按钮变为「继续处理（N%）」
    await refreshFileStatus();
  };

  const doPause = async () => {
    if (!taskRunning) return;
    setPausing(true);
    try {
      await backend.pauseFileProcessing();
      stopPolling();
      await refreshAfterPause();
    } catch (e) {
      message.error(`暂停失败：${e}`);
    } finally {
      setPausing(false);
    }
  };

  const onPause = () => {
    if (!taskRunning) return;
    // 提取/转写阶段无句级断点，暂停即放弃本次转写（续跑重新上传+转写）
    if (phase === "extracting" || phase === "transcribing") {
      modal.confirm({
        title: "暂停将放弃本次转写",
        content:
          "音频提取与转写无法断点保存，暂停后继续将重新上传音频并重新转写（费用重付）。确定暂停吗？",
        okText: "暂停",
        cancelText: "取消",
        onOk: doPause,
      });
      return;
    }
    doPause();
  };

  const doStop = async () => {
    if (!file) return;
    try {
      await backend.stopFileProcessing(file.path);
      stopPolling();
      setTask(null);
      setFileStatus({ state: "fresh", percent: 0 });
    } catch (e) {
      message.error(`停止失败：${e}`);
    }
  };

  const onStop = () => {
    if (!file) return;
    modal.confirm({
      title: "停止并清除进度？",
      content:
        "已有进度将清零且不可恢复，已产生的转写与翻译费用不会退还。确定停止吗？",
      okText: "停止并清零",
      okButtonProps: { danger: true },
      cancelText: "取消",
      onOk: doStop,
    });
  };

  const onExport = async (format: "srt" | "vtt") => {
    setExporting(format);
    try {
      const path = await backend.exportSubtitle(format);
      message.success({ content: `已导出：${path}`, duration: 8 });
      setTask((t) =>
        t ? { ...t, progress: { ...t.progress, state: "exported" } } : t,
      );
    } catch (e) {
      if (`${e}`.includes("Save cancelled")) {
        message.info("已取消保存");
      } else {
        message.error(`导出失败：${e}`);
      }
    } finally {
      setExporting(null);
    }
  };

  const pct =
    task === null ? 0 : Math.max(0, Math.min(100, Math.round(task.progress.progress * 100)));
  const done =
    task !== null &&
    (task.progress.state === "completed" || task.progress.state === "exported");
  const failed = task !== null && task.progress.state === "failed";
  // 可续传任务的语言被手动改过 → 续传失效，退回全量重跑语义
  const langChanged =
    fileStatus?.state === "translating" &&
    !!fileStatus.source_language &&
    fileStatus.source_language !== language;

  const phase = task?.progress.phase ?? "idle";
  const stepIndex = !task
    ? -1
    : task.progress.state === "idle"
      ? 0
      : phase === "extracting"
        ? 0
        : phase === "transcribing"
          ? 1
          : phase === "translating"
            ? 2
            : 3;

  const formatRelativeTime = (ts: number): string => {
    const diff = Math.floor(Date.now() / 1000) - ts;
    if (diff < 3600) return `${Math.max(1, Math.floor(diff / 60))} 分钟前`;
    if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
    return `${Math.floor(diff / 86400)} 天前`;
  };

  const gradientStroke = { "0%": "#6366f1", "100%": "#8b5cf6" } as const;

  return (
    <div className="view" aria-hidden={!active}>
      {!file ? (
        <>
          <div
            className={`drop-card ${dragOver ? "over" : ""}`}
            role="button"
            tabIndex={0}
            aria-label="选择或拖入视频文件"
            onClick={onPickFile}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") onPickFile();
            }}
          >
            <div className="drop-icon">
              <InboxOutlined style={{ fontSize: 24, color: "#fff" }} />
            </div>
            <div>
              <div style={{ fontSize: 15, fontWeight: 600 }}>
                {dragOver ? "松开即可选择" : "拖入视频文件，或点击选择"}
              </div>
              <div style={{ fontSize: 12, color: token.colorTextTertiary, marginTop: 4 }}>
                自动提取音频、转写并翻译为双语字幕，导出 SRT / VTT
              </div>
            </div>
            <div className="drop-formats mono">
              {["MP4", "MKV", "AVI", "MOV"].map((f) => (
                <span key={f} className="fmt">
                  {f}
                </span>
              ))}
            </div>
          </div>

          {recentTasks.length > 0 && (
            <div className="recent-frame">
              <div className="recent-label mono">最近处理</div>
              {recentTasks.map((rt) => (
                <div
                  key={rt.task_id}
                  className="recent-row"
                  role="button"
                  tabIndex={0}
                  onClick={() => selectFile(rt.video_path)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") selectFile(rt.video_path);
                  }}
                >
                  <VideoCameraOutlined
                    style={{ fontSize: 15, color: PRIMARY, flex: "none" }}
                  />
                  <span className="recent-name">{rt.file_name}</span>
                  <span className="recent-time mono">{formatRelativeTime(rt.modified_at)}</span>
                  <span className={`recent-badge ${rt.state}`}>
                    {rt.state === "completed"
                      ? "已完成"
                      : rt.state === "translating"
                        ? `翻译中断 · ${Math.round(rt.percent * 100)}%`
                        : "未开始"}
                  </span>
                </div>
              ))}
            </div>
          )}
        </>
      ) : (
        <>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <Tooltip title="返回主页">
              <Button
                type="text"
                icon={<ArrowLeftOutlined />}
                onClick={onBack}
                disabled={taskRunning}
                aria-label="返回主页"
              />
            </Tooltip>
            <div
              style={{
                width: 38,
                height: 38,
                borderRadius: 11,
                flex: "none",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                background: "#eef2ff",
              }}
            >
              <VideoCameraOutlined style={{ fontSize: 17, color: PRIMARY }} />
            </div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <Typography.Text strong ellipsis style={{ fontSize: 13.5, display: "block" }}>
                {file.name}
              </Typography.Text>
              <div
                className="mono"
                style={{
                  fontSize: 10.5,
                  color: token.colorTextTertiary,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {file.path}
              </div>
            </div>
            {!taskRunning && (
              <Tooltip title="重新选择文件">
                <Button
                  type="text"
                  icon={<ReloadOutlined />}
                  onClick={onPickFile}
                  aria-label="重新选择文件"
                />
              </Tooltip>
            )}
            <Select
              value={language}
              onChange={setLanguage}
              options={LANGUAGES}
              style={{ width: 72 }}
              aria-label="视频语言"
              disabled={taskRunning}
            />
            {!taskRunning && (
              <Tooltip
                title={
                  task && done
                    ? "重新处理"
                    : fileStatus?.state === "translating"
                      ? "继续处理"
                      : fileStatus?.state === "completed"
                        ? "重新处理"
                        : "开始转字幕"
                }
              >
                <Button
                  type="primary"
                  icon={<PlayCircleOutlined />}
                  loading={starting}
                  onClick={onStart}
                  aria-label="开始或继续处理"
                />
              </Tooltip>
            )}
            {!taskRunning && fileStatus?.state === "translating" && (
              <Tooltip title="清除进度">
                <Button
                  danger
                  type="text"
                  icon={<DeleteOutlined />}
                  onClick={onStop}
                  aria-label="清除进度"
                />
              </Tooltip>
            )}
          </div>

          {task && (
            <div className="task-panel">
              <div className="task-inner">
                <Steps
                  size="small"
                  current={stepIndex}
                  status={failed ? "error" : undefined}
                  items={[
                    { title: "提取音频" },
                    { title: "转写" },
                    { title: "翻译" },
                    { title: "完成" },
                  ]}
                  style={{ marginBottom: 18 }}
                />

                <div
                  style={{ display: "flex", alignItems: "baseline", gap: 4, marginBottom: 12 }}
                >
                  <span className="pct grad-text mono">{pct}</span>
                  <span className="pct-sign mono">%</span>
                  <span
                    style={{
                      marginLeft: "auto",
                      fontSize: 12,
                      color: failed ? token.colorError : token.colorTextTertiary,
                    }}
                  >
                    {STATUS_LABEL[task.progress.state]}
                  </span>
                </div>

                <Progress
                  percent={pct}
                  showInfo={false}
                  strokeColor={failed ? undefined : gradientStroke}
                  status={
                    failed
                      ? "exception"
                      : done
                        ? "success"
                        : task?.progress.state === "paused"
                          ? "normal"
                          : "active"
                  }
                />
                <div className="task-status-line">
                  {failed ? (
                    <Typography.Text type="danger" style={{ fontSize: 12.5 }}>
                      {task.progress.error ?? "未知错误"}
                    </Typography.Text>
                  ) : task.progress.state === "processing" ? (
                    "正在转写与翻译…"
                  ) : task.progress.state === "idle" ? (
                    "正在准备…"
                  ) : task.progress.state === "paused" ? (
                    fileStatus?.state === "translating" ? (
                      "已暂停，进度已保留，可随时继续"
                    ) : (
                      "已暂停，本次转写未保留，继续将重新上传并转写"
                    )
                  ) : done ? (
                    "转写完成，可导出字幕文件"
                  ) : null}
                </div>

                {taskRunning && (
                  <div className="task-actions">
                    <Button
                      icon={<PauseCircleOutlined />}
                      loading={pausing}
                      onClick={onPause}
                    >
                      暂停
                    </Button>
                    <Button danger icon={<StopOutlined />} onClick={onStop}>
                      停止
                    </Button>
                  </div>
                )}

                {done && (
                  <div className="task-actions">
                    <Button
                      type="primary"
                      icon={<DownloadOutlined />}
                      loading={exporting === "srt"}
                      onClick={() => onExport("srt")}
                    >
                      导出 SRT
                    </Button>
                    <Button
                      icon={<DownloadOutlined />}
                      loading={exporting === "vtt"}
                      onClick={() => onExport("vtt")}
                    >
                      导出 VTT
                    </Button>
                  </div>
                )}
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}

