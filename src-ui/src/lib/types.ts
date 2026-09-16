export interface AsrConfig {
  provider: string;
  api_key: string;
  workspace_id: string | null;
  file_model: string;
  realtime_model: string;
}

export interface TranslateConfig {
  provider: string;
  model: string;
  api_key: string;
  target_lang: string;
}

export interface OssConfig {
  endpoint: string;
  bucket: string;
  access_key_id: string;
  access_key_secret: string;
  path_prefix: string | null;
}

export interface AppConfig {
  asr: AsrConfig;
  translate: TranslateConfig;
  oss: OssConfig | null;
}

export type PipelineStateName =
  | "idle"
  | "processing"
  | "completed"
  | "exported"
  | "failed";

export type PhaseName = "idle" | "extracting" | "transcribing" | "translating" | "done";

export interface ProgressInfo {
  state: PipelineStateName;
  progress: number;
  error: string | null;
  phase: PhaseName;
}

export type RecentTaskState = "fresh" | "translating" | "completed";

export interface RecentTask {
  task_id: string;
  video_path: string;
  file_name: string;
  modified_at: number;
  state: RecentTaskState;
  percent: number;
}

export interface TaskStatus {
  state: RecentTaskState;
  percent: number;
  /** checkpoint 记录的源语言，仅 translating/completed 返回 */
  source_language?: string;
}

export function basename(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i >= 0 ? path.slice(i + 1) : path;
}

export function defaultConfig(): AppConfig {
  return {
    asr: {
      provider: "dashscope",
      api_key: "",
      workspace_id: null,
      file_model: "qwen-audio-3.0-asr-flash-filetrans",
      realtime_model: "qwen-audio-3.0-asr-flash",
    },
    translate: {
      provider: "openai",
      model: "gpt-3.5-turbo",
      api_key: "",
      target_lang: "zh",
    },
    oss: null,
  };
}
