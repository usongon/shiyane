import { createContext } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AppConfig,
  ProgressInfo,
  RecentTask,
  TaskStatus,
  CaptureTarget,
  RealtimeStateInfo,
  SubtitlePartialEvent,
  SubtitleFinalEvent,
  TranslationEvent,
  RealtimeStateEvent,
} from "./types";
import { mockBackend } from "./mock";

export interface DragHandlers {
  onEnter?: () => void;
  onLeave?: () => void;
  onDrop?: (paths: string[]) => void;
}

export interface Backend {
  readonly mocked: boolean;
  getConfig(): Promise<AppConfig>;
  saveConfig(config: AppConfig): Promise<void>;
  getHostPlatform(): Promise<"macos" | "windows">;
  startFileProcessing(videoPath: string, sourceLanguage: string): Promise<string>;
  pauseFileProcessing(): Promise<void>;
  stopFileProcessing(videoPath: string): Promise<void>;
  deleteTask(taskId: string): Promise<void>;
  getProcessingProgress(): Promise<ProgressInfo>;
  exportSubtitle(format: "srt" | "vtt"): Promise<string>;
  testAsrConnection(config: AppConfig): Promise<string>;
  testTranslateConnection(config: AppConfig): Promise<string>;
  listRecentTasks(): Promise<RecentTask[]>;
  getTaskStatus(videoPath: string): Promise<TaskStatus>;
  pickVideoFile(): Promise<string | null>;
  onDragEvent(handlers: DragHandlers): Promise<() => void>;

  // Realtime subtitle
  startRealtime(sourceLanguage: string, targetIds: string[]): Promise<string>;
  pauseRealtime(): Promise<void>;
  resumeRealtime(): Promise<void>;
  stopRealtime(): Promise<void>;
  listCaptureTargets(): Promise<CaptureTarget[]>;
  getRealtimeState(): Promise<RealtimeStateInfo>;
  toggleRealtimeOverlay(): Promise<boolean>;

  onSubtitlePartial(cb: (e: SubtitlePartialEvent) => void): Promise<() => void>;
  onSubtitleFinal(cb: (e: SubtitleFinalEvent) => void): Promise<() => void>;
  onTranslation(cb: (e: TranslationEvent) => void): Promise<() => void>;
  onRealtimeStateChange(cb: (e: RealtimeStateEvent) => void): Promise<() => void>;
}

export const BackendContext = createContext<Backend>(mockBackend);

const isTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const tauriBackend: Backend = {
  mocked: false,
  getConfig: () => invoke<AppConfig>("get_config"),
  saveConfig: (config) => invoke<void>("save_config", { config }),
  getHostPlatform: () => invoke<"macos" | "windows">("get_host_platform"),
  startFileProcessing: (videoPath, sourceLanguage) =>
    invoke<string>("start_file_processing", { videoPath, sourceLanguage }),
  pauseFileProcessing: () => invoke<void>("pause_file_processing"),
  stopFileProcessing: (videoPath) =>
    invoke<void>("stop_file_processing", { videoPath }),
  deleteTask: (taskId) => invoke<void>("delete_task", { taskId }),
  getProcessingProgress: () => invoke<ProgressInfo>("get_processing_progress"),
  exportSubtitle: (format) => invoke<string>("export_subtitle", { format }),
  testAsrConnection: (config) => invoke<string>("test_asr_connection", { config }),
  testTranslateConnection: (config) =>
    invoke<string>("test_translate_connection", { config }),
  listRecentTasks: () => invoke<RecentTask[]>("list_recent_tasks"),
  getTaskStatus: (videoPath) =>
    invoke<TaskStatus>("get_task_status", { videoPath }),
  pickVideoFile: async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "视频", extensions: ["mp4", "mkv", "avi", "mov"] }],
    });
    return typeof path === "string" ? path : null;
  },
  onDragEvent: async (handlers) => {
    const unlistens = await Promise.all([
      listen<{ paths: string[] }>("tauri://drag-drop", (e) =>
        handlers.onDrop?.(e.payload?.paths ?? []),
      ),
      listen("tauri://drag-enter", () => handlers.onEnter?.()),
      listen("tauri://drag-leave", () => handlers.onLeave?.()),
    ]);
    return () => unlistens.forEach((u) => u());
  },

  startRealtime: (sourceLanguage, targetIds) =>
    invoke<string>("start_realtime_session", { sourceLanguage, captureTargetIds: targetIds }),
  pauseRealtime: () => invoke<void>("pause_realtime_session"),
  resumeRealtime: () => invoke<void>("resume_realtime_session"),
  stopRealtime: () => invoke<void>("stop_realtime_session"),
  listCaptureTargets: () => invoke<CaptureTarget[]>("list_capture_targets"),
  getRealtimeState: () => invoke<RealtimeStateInfo>("get_realtime_state"),
  toggleRealtimeOverlay: () => invoke<boolean>("toggle_realtime_overlay"),

  onSubtitlePartial: async (cb) => {
    const unlisten = await listen<SubtitlePartialEvent>("realtime:subtitle-partial", (e) => cb(e.payload));
    return unlisten;
  },
  onSubtitleFinal: async (cb) => {
    const unlisten = await listen<SubtitleFinalEvent>("realtime:subtitle-final", (e) => cb(e.payload));
    return unlisten;
  },
  onTranslation: async (cb) => {
    const unlisten = await listen<TranslationEvent>("realtime:translation", (e) => cb(e.payload));
    return unlisten;
  },
  onRealtimeStateChange: async (cb) => {
    const unlisten = await listen<RealtimeStateEvent>("realtime:state-change", (e) => cb(e.payload));
    return unlisten;
  },
};

export function getBackend(): Backend {
  return isTauri ? tauriBackend : mockBackend;
}
