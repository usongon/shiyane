import type {
  AppConfig,
  ProgressInfo,
  RecentTask,
  CaptureTarget,
  RealtimeStateInfo,
} from "./types";
import type { Backend, DragHandlers } from "./backend";

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

let config: AppConfig = {
  asr: {
    provider: "dashscope",
    api_key: "sk-demo-xxxxxxxxxxxxxxxx",
    workspace_id: "llm-demoxxxxxxxx",
    file_model: "qwen-audio-3.0-asr-flash-filetrans",
    realtime_model: "qwen-audio-3.0-asr-flash-streaming",
  },
  translate: {
    provider: "openai",
    model: "gpt-3.5-turbo",
    api_key: "sk-demo-xxxxxxxxxxxxxxxx",
    target_lang: "zh",
  },
  oss: {
    endpoint: "oss-cn-beijing.aliyuncs.com",
    bucket: "shiyane-demo-bucket",
    access_key_id: "LTAI-demo",
    access_key_secret: "demo-secret",
    path_prefix: "shiyane-temp/",
  },
};

let progress: ProgressInfo = { state: "idle", progress: 0, error: null, phase: "idle" };
let timer: number | null = null;
const deletedVideoPaths = new Set<string>();
const deletedTaskIds = new Set<string>();

function stopTimer() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}

export const mockBackend: Backend = {
  mocked: true,
  async getConfig() {
    await delay(250);
    return structuredClone(config);
  },
  async saveConfig(c) {
    await delay(350);
    config = structuredClone(c);
  },
  async startFileProcessing(_videoPath, _sourceLanguage) {
    await delay(400);
    stopTimer();
    progress = { state: "processing", progress: 0.02, error: null, phase: "extracting" };
    // 模拟：提取音频 → 转写 → 翻译逐段推进，用于浏览器演示全流程与 Steps 阶段显示
    timer = window.setInterval(() => {
      if (progress.state !== "processing") {
        stopTimer();
        return;
      }
      if (progress.phase === "extracting") {
        progress = { state: "processing", progress: 0.05, error: null, phase: "transcribing" };
        return;
      }
      if (progress.phase === "transcribing") {
        progress = { state: "processing", progress: 0.08, error: null, phase: "translating" };
        return;
      }
      const next = Math.min(1, progress.progress + 0.03 + Math.random() * 0.02);
      progress =
        next >= 1
          ? { state: "completed", progress: 1, error: null, phase: "done" }
          : { state: "processing", progress: next, error: null, phase: "translating" };
    }, 400);
    return "demo-task";
  },
  async pauseFileProcessing() {
    await delay(300);
    stopTimer();
    if (progress.state === "processing" || progress.state === "idle") {
      progress = { ...progress, state: "paused" };
    }
  },
  async stopFileProcessing(videoPath) {
    await delay(300);
    stopTimer();
    progress = { state: "idle", progress: 0, error: null, phase: "idle" };
    deletedVideoPaths.add(videoPath);
  },
  async deleteTask(taskId) {
    await delay(250);
    deletedTaskIds.add(taskId);
  },
  async getProcessingProgress() {
    return { ...progress };
  },
  async exportSubtitle(format) {
    await delay(700);
    return `/Users/demo/Downloads/output.${format}`;
  },
  async testAsrConnection() {
    await delay(900);
    return "ASR 配置验证通过（演示模式）";
  },
  async testTranslateConnection() {
    await delay(900);
    return "翻译连接成功（演示模式）";
  },
  async listRecentTasks(): Promise<RecentTask[]> {
    await delay(300);
    const now = Math.floor(Date.now() / 1000);
    const tasks: RecentTask[] = [
      {
        task_id: "a1b2c3",
        video_path: "/Users/demo/Movies/tears_of_steel_1080p.mp4",
        file_name: "tears_of_steel_1080p.mp4",
        modified_at: now - 3600,
        state: "completed",
        percent: 1,
        task_type: "file",
      },
      {
        task_id: "d4e5f6",
        video_path: "/Users/demo/Movies/product_demo_final.mp4",
        file_name: "product_demo_final.mp4",
        modified_at: now - 86400,
        state: "translating",
        percent: 0.45,
        task_type: "file",
      },
      {
        task_id: "g7h8i9",
        video_path: "/Users/demo/Movies/meeting_recording_0912.mkv",
        file_name: "meeting_recording_0912.mkv",
        modified_at: now - 86400 * 3,
        state: "fresh",
        percent: 0,
        task_type: "file",
      },
    ];
    return tasks.filter(
      (t) => !deletedVideoPaths.has(t.video_path) && !deletedTaskIds.has(t.task_id),
    );
  },
  async getTaskStatus(videoPath) {
    await delay(200);
    if (videoPath.includes("tears_of_steel"))
      return { state: "completed", percent: 1, source_language: "en" };
    if (videoPath.includes("product_demo"))
      return { state: "translating", percent: 0.45, source_language: "ja" };
    return { state: "fresh", percent: 0 };
  },
  async pickVideoFile() {
    await delay(350);
    return "/Users/demo/Movies/tears_of_steel_1080p.mp4";
  },
  async onDragEvent(handlers: DragHandlers) {
    // 浏览器内收不到 Tauri 系统级拖拽事件；暴露到 window 上便于 devtools 手动模拟
    (window as unknown as Record<string, unknown>).__mockDrag = handlers;
    return () => {
      delete (window as unknown as Record<string, unknown>).__mockDrag;
    };
  },

  async startRealtime(_sourceLanguage, _targetIds) {
    await delay(400);
    return "realtime-demo-session";
  },
  async pauseRealtime() {
    await delay(200);
  },
  async resumeRealtime() {
    await delay(200);
  },
  async stopRealtime() {
    await delay(300);
  },
  async listCaptureTargets(): Promise<CaptureTarget[]> {
    await delay(300);
    return [
      { id: "chrome", name: "Google Chrome", kind: "system_audio", icon_path: null },
      { id: "spotify", name: "Spotify", kind: "system_audio", icon_path: null },
      { id: "mic", name: "内置麦克风", kind: "microphone", icon_path: null },
    ];
  },
  async getRealtimeState(): Promise<RealtimeStateInfo> {
    return { state: "idle", session_id: null, entry_count: 0, error: null };
  },

  async onSubtitlePartial(_cb) {
    return () => {};
  },
  async onSubtitleFinal(_cb) {
    return () => {};
  },
  async onTranslation(_cb) {
    return () => {};
  },
  async onRealtimeStateChange(_cb) {
    return () => {};
  },
};
