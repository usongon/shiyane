pub mod realtime;

use pick_up_sound_text::asr::DashScopeFileTransProvider;
use pick_up_sound_text::audio::FileAudioSource;
use pick_up_sound_text::checkpoint::{Checkpoint, CheckpointFingerprint, compute_task_id};
use pick_up_sound_text::config::{AppConfig, translate_provider_preset};
use pick_up_sound_text::pipeline::{FilePipeline, PipelineState};
use pick_up_sound_text::subtitle::{SubtitleEntry, generate_srt, generate_vtt};
use pick_up_sound_text::translate::{OpenAiCompatibleProvider, TranslateProvider};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Serialize)]
pub struct ProgressInfo {
    pub state: String,
    pub progress: f64,
    pub error: Option<String>,
    pub phase: String,
}

#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Fresh,
    Translating,
    Completed,
}

#[derive(Serialize)]
pub struct TaskStatus {
    pub state: TaskState,
    pub percent: f64,
    /// checkpoint 记录的源语言；仅 translating/completed 有值。
    /// 前端用它把语言下拉自动选回原值，避免「点了继续却因语言不同全量重跑」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_language: Option<String>,
    /// 翻译重试用尽、回退为原文的句数（完成态提示用；0 或未完成时不返回）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_count: Option<usize>,
}

#[derive(Serialize)]
pub struct RecentTask {
    pub task_id: String,
    pub video_path: String,
    pub file_name: String,
    pub modified_at: u64,
    pub state: TaskState,
    pub percent: f64,
    /// 任务类型：file（文件转字幕）或 realtime（实时会话）
    pub task_type: String,
}

pub struct AppState {
    pub pipeline_state: Arc<Mutex<Option<Arc<Mutex<PipelineState>>>>>,
    pub pipeline_entries: Arc<Mutex<Option<Arc<Mutex<Vec<SubtitleEntry>>>>>>,
    pub processing_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub config: Arc<Mutex<AppConfig>>,
    pub pipeline: Arc<Mutex<Option<FilePipeline>>>,
    pub pipeline_progress: Arc<Mutex<Option<Arc<Mutex<f64>>>>>,
    pub pipeline_phase: Arc<Mutex<Option<Arc<Mutex<pick_up_sound_text::pipeline::Phase>>>>>,
    /// 当前运行任务的取消句柄与视频路径（暂停/停止用；无运行任务时为 None）
    pub cancel_token: Arc<Mutex<Option<CancellationToken>>>,
    pub running_video_path: Arc<Mutex<Option<String>>>,
    pub running_task_id: Arc<Mutex<Option<String>>>,
    /// 串行化 start/pause/stop：交错调用也不会出现「旧任务句柄被新任务清除」等竞态
    pub control: Arc<Mutex<()>>,
    /// 实时会话状态（单一 Mutex 包裹，避免多 Mutex 死锁）
    pub realtime: Arc<Mutex<RealtimeSessionInner>>,
}

/// 所有任务 checkpoint 的根目录（$APP_DATA/tasks）
fn tasks_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tasks"))
}

/// 取消运行中的任务并等待退出，随后清空全部运行句柄。
/// 调用前提：调用方已确认目标就是当前运行任务（路径或 task_id 匹配）。
async fn halt_running_task(state: &AppState) {
    let token = state.cancel_token.lock().await.clone();
    if let Some(t) = token.as_ref() {
        t.cancel();
    }
    let handle = state.processing_task.lock().await.take();
    if let Some(h) = handle {
        let _ = h.await;
    }
    *state.pipeline_state.lock().await = None;
    *state.pipeline_entries.lock().await = None;
    *state.pipeline_progress.lock().await = None;
    *state.pipeline_phase.lock().await = None;
    *state.cancel_token.lock().await = None;
    *state.running_video_path.lock().await = None;
    *state.running_task_id.lock().await = None;
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    let config = state.config.lock().await;
    Ok(config.clone())
}

#[tauri::command]
pub async fn save_config(config: AppConfig, state: State<'_, AppState>) -> Result<(), String> {
    let mut config_guard = state.config.lock().await;
    *config_guard = config.clone();
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn start_file_processing(
    video_path: String,
    source_language: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    tracing::info!(
        "start_file_processing called: video_path={}, source_language={}",
        video_path,
        source_language
    );

    let _control = state.control.lock().await;
    let video_path = PathBuf::from(video_path);

    // Get config
    let config = state.config.lock().await.clone();

    // Create audio source
    let audio_source = FileAudioSource::new(video_path.clone())
        .await
        .map_err(|e| e.to_string())?;

    // Create ASR provider with OSS config
    let asr_provider = DashScopeFileTransProvider {
        oss_config: config.oss.clone(),
    };

    // Create translate provider using preset base_url
    let (base_url, _) = translate_provider_preset(&config.translate.provider);
    let translate_provider = OpenAiCompatibleProvider {
        base_url: base_url.to_string(),
        model: config.translate.model.clone(),
        api_key: config.translate.api_key.clone(),
        // 实测正常单句可达 45s+（代理 + 长上下文），60s 会误杀慢而活的请求
        timeout_secs: 90,
    };

    // Create pipeline
    let mut pipeline = FilePipeline::new(
        Box::new(audio_source),
        Box::new(asr_provider),
        Box::new(translate_provider),
        config.clone(),
        source_language.clone(),
    );

    // Stable task_id（path+size+mtime）：同文件重跑续传，换文件天然隔离
    let task_id = compute_task_id(&video_path).map_err(|e| e.to_string())?;

    let app_data_dir = tasks_root(&app)?;

    let fingerprint = CheckpointFingerprint {
        source_language: source_language.clone(),
        target_lang: config.translate.target_lang.clone(),
        translate_provider: config.translate.provider.clone(),
        translate_model: config.translate.model.clone(),
        asr_model: config.asr.file_model.clone(),
    };
    pipeline
        .init_checkpoint_in(
            app_data_dir,
            task_id.clone(),
            video_path.clone(),
            fingerprint,
        )
        .map_err(|e| e.to_string())?;

    // Get shared state handles
    let state_handle = pipeline.state_handle();
    let entries_handle = pipeline.entries_handle();
    let progress_handle = pipeline.progress_handle();

    // Store handles in app state
    let mut pipeline_state_guard = state.pipeline_state.lock().await;
    *pipeline_state_guard = Some(state_handle.clone());
    drop(pipeline_state_guard);

    let mut pipeline_entries_guard = state.pipeline_entries.lock().await;
    *pipeline_entries_guard = Some(entries_handle.clone());
    drop(pipeline_entries_guard);

    let mut pipeline_progress_guard = state.pipeline_progress.lock().await;
    *pipeline_progress_guard = Some(progress_handle);
    drop(pipeline_progress_guard);

    let phase_handle = pipeline.phase_handle();
    let mut pipeline_phase_guard = state.pipeline_phase.lock().await;
    *pipeline_phase_guard = Some(phase_handle);
    drop(pipeline_phase_guard);

    // 登记取消句柄与运行路径（先于 spawn，杜绝暂停/停止竞态）
    let cancel_handle = pipeline.cancel_handle();
    let mut cancel_guard = state.cancel_token.lock().await;
    *cancel_guard = Some(cancel_handle);
    drop(cancel_guard);

    let mut running_guard = state.running_video_path.lock().await;
    *running_guard = Some(video_path.to_string_lossy().to_string());
    drop(running_guard);

    let mut running_id_guard = state.running_task_id.lock().await;
    *running_id_guard = Some(task_id.clone());
    drop(running_id_guard);

    // Store pipeline reference
    let mut pipeline_guard = state.pipeline.lock().await;
    *pipeline_guard = Some(pipeline);
    drop(pipeline_guard);

    // Start processing in background
    let pipeline_clone = state.pipeline.clone();
    let processing_task = tokio::spawn(async move {
        let mut pipeline_guard = pipeline_clone.lock().await;
        if let Some(pipeline) = pipeline_guard.as_mut() {
            if let Err(e) = pipeline.process().await {
                tracing::error!("Pipeline error: {}", e);
            }
        }
    });

    // Store task handle
    let mut task_guard = state.processing_task.lock().await;
    *task_guard = Some(processing_task);
    drop(task_guard);

    Ok(task_id)
}

#[tauri::command]
pub async fn get_processing_progress(state: State<'_, AppState>) -> Result<ProgressInfo, String> {
    let phase = {
        let guard = state.pipeline_phase.lock().await;
        match guard.as_ref() {
            Some(h) => h.lock().await.as_str().to_string(),
            None => "idle".to_string(),
        }
    };

    let pipeline_state_guard = state.pipeline_state.lock().await;
    if let Some(state_handle) = pipeline_state_guard.as_ref() {
        let pipeline_state = state_handle.lock().await.clone();
        let progress_guard = state.pipeline_progress.lock().await;
        let progress_value = if let Some(progress_handle) = progress_guard.as_ref() {
            *progress_handle.lock().await
        } else {
            0.0
        };

        let (state_str, progress, error) = match pipeline_state {
            PipelineState::Idle => ("idle", progress_value, None),
            PipelineState::Processing => ("processing", progress_value, None),
            PipelineState::Paused => ("paused", progress_value, None),
            PipelineState::Completed => ("completed", 1.0, None),
            PipelineState::Exported => ("exported", 1.0, None),
            PipelineState::Failed(err) => ("failed", progress_value, Some(err)),
        };

        Ok(ProgressInfo {
            state: state_str.to_string(),
            progress,
            error,
            phase,
        })
    } else {
        Ok(ProgressInfo {
            state: "idle".to_string(),
            progress: 0.0,
            error: None,
            phase: "idle".to_string(),
        })
    }
}

#[tauri::command]
pub async fn pause_file_processing(state: State<'_, AppState>) -> Result<(), String> {
    let _control = state.control.lock().await;
    let token = state.cancel_token.lock().await.clone();
    if let Some(t) = token.as_ref() {
        t.cancel();
    }
    // 等待管线退出，保证返回时状态已定格为 Paused（无运行任务则瞬时返回）
    let handle = state.processing_task.lock().await.take();
    if let Some(h) = handle {
        let _ = h.await;
    }
    Ok(())
}

#[tauri::command]
pub async fn stop_file_processing(
    video_path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _control = state.control.lock().await;
    let running = state.running_video_path.lock().await.clone();
    let was_running = running.as_deref() == Some(video_path.as_str());

    if was_running {
        halt_running_task(&state).await;
    }

    // 归零：删除该文件的整个 checkpoint 目录（运行中与未运行同样适用）。
    // 视频文件已删除 = 无可清任务，静默成功（与 get_task_status 的 NotFound 语义一致）
    let path = PathBuf::from(&video_path);
    let task_id = match compute_task_id(&path) {
        Ok(id) => id,
        Err(pick_up_sound_text::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(e) => return Err(e.to_string()),
    };
    let task_dir = tasks_root(&app)?.join(&task_id);
    if task_dir.exists() {
        std::fs::remove_dir_all(&task_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 按 task_id 精确删除任务（最近列表用）。
/// stop_file_processing 按路径重算 task_id，对「视频已被替换/移动」或
/// 历史遗留目录（相对路径）会算错目标；列表行自带真实 task_id，直接命中。
#[tauri::command]
pub async fn delete_task(
    task_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _control = state.control.lock().await;
    let running_id = state.running_task_id.lock().await.clone();
    if running_id.as_deref() == Some(task_id.as_str()) {
        halt_running_task(&state).await;
    }
    let task_dir = tasks_root(&app)?.join(&task_id);
    if task_dir.exists() {
        std::fs::remove_dir_all(&task_dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn export_subtitle(
    format: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let entries = {
        let guard = state.pipeline_entries.lock().await;
        match guard.as_ref() {
            Some(handle) => handle.lock().await.clone(),
            None => return Err("No pipeline available".to_string()),
        }
    };

    let (content, ext, title) = match format.as_str() {
        "srt" => (generate_srt(&entries), "srt", "Save SRT subtitle"),
        "vtt" => (generate_vtt(&entries), "vtt", "Save VTT subtitle"),
        _ => return Err(format!("Unsupported format: {}", format)),
    };

    let file_path = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(format!("output.{}", ext))
        .add_filter(format!("{} Subtitle", ext.to_uppercase()), &[ext])
        .blocking_save_file();

    if let Some(path) = file_path {
        let path_buf = path.into_path().map_err(|e| e.to_string())?;
        std::fs::write(&path_buf, content).map_err(|e| e.to_string())?;
        Ok(path_buf.to_string_lossy().to_string())
    } else {
        Err("Save cancelled".to_string())
    }
}

#[tauri::command]
pub async fn test_asr_connection(config: AppConfig) -> Result<String, String> {
    tracing::info!("test_asr_connection called");
    tracing::info!("ASR provider: {}", config.asr.provider);
    tracing::info!("ASR api_key length: {} chars", config.asr.api_key.len());
    tracing::info!("ASR workspace_id: {:?}", config.asr.workspace_id);
    tracing::info!("OSS config present: {}", config.oss.is_some());

    // Validate required config
    if config.asr.api_key.is_empty() {
        return Err("ASR API Key 未配置".to_string());
    }

    if config.asr.workspace_id.is_none() {
        return Err("ASR Workspace ID 未配置（北京区域必填）".to_string());
    }

    if config.oss.is_none() {
        return Err("OSS 配置未设置（文件转写需要 OSS 托管音频）".to_string());
    }

    let oss = config.oss.as_ref().unwrap();
    if oss.endpoint.is_empty()
        || oss.bucket.is_empty()
        || oss.access_key_id.is_empty()
        || oss.access_key_secret.is_empty()
    {
        return Err("OSS 配置不完整".to_string());
    }

    // Try to create provider and validate OSS connection
    let _provider = DashScopeFileTransProvider {
        oss_config: config.oss.clone(),
    };

    // For now, just validate config presence
    // TODO: Could try uploading a small test file to OSS to verify credentials
    Ok("ASR 配置验证通过（API Key、Workspace ID、OSS 配置已设置）".to_string())
}

#[tauri::command]
pub async fn test_translate_connection(config: AppConfig) -> Result<String, String> {
    let (base_url, _) = translate_provider_preset(&config.translate.provider);
    let provider = OpenAiCompatibleProvider {
        base_url: base_url.to_string(),
        model: config.translate.model.clone(),
        api_key: config.translate.api_key.clone(),
        timeout_secs: 30,
    };

    match provider.test_connection().await {
        Ok(_) => Ok("翻译连接成功".to_string()),
        Err(e) => Err(format!("翻译连接失败: {}", e)),
    }
}

#[tauri::command]
pub async fn get_task_status(
    video_path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<TaskStatus, String> {
    // 实时会话：video_path 是 "realtime://session_id"，task_id 就是 session_id
    if video_path.starts_with("realtime://") {
        let task_id = video_path.strip_prefix("realtime://").unwrap_or(&video_path);
        let cp_path = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("tasks")
            .join(task_id)
            .join("progress.jsonl");

        let cp = match Checkpoint::load(&cp_path).map_err(|e| e.to_string())? {
            None => return Ok(TaskStatus { state: TaskState::Fresh, percent: 0.0, source_language: None, fallback_count: None }),
            Some(c) => c,
        };

        if cp.segments.is_empty() {
            return Ok(TaskStatus { state: TaskState::Fresh, percent: 0.0, source_language: None, fallback_count: None });
        } else if cp.is_all_completed() {
            let fallback_count = cp.segments.iter().filter(|s| s.fallback).count();
            return Ok(TaskStatus {
                state: TaskState::Completed,
                percent: 1.0,
                source_language: Some(cp.fingerprint.source_language.clone()),
                fallback_count: (fallback_count > 0).then_some(fallback_count),
            });
        } else {
            return Ok(TaskStatus {
                state: TaskState::Translating,
                percent: cp.percent(),
                source_language: Some(cp.fingerprint.source_language.clone()),
                fallback_count: None,
            });
        }
    }

    // 文件任务：原有逻辑
    let path = PathBuf::from(video_path);
    let task_id = match compute_task_id(&path) {
        Ok(id) => id,
        // 文件不存在 = 无可续传任务，Fresh 语义更诚实（其余错误仍透传）
        Err(pick_up_sound_text::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TaskStatus {
                state: TaskState::Fresh,
                percent: 0.0,
                source_language: None,
                fallback_count: None,
            });
        }
        Err(e) => return Err(e.to_string()),
    };
    let cp_path = tasks_root(&app)?.join(&task_id).join("progress.jsonl");

    let cp = match Checkpoint::load(&cp_path).map_err(|e| e.to_string())? {
        None => {
            return Ok(TaskStatus {
                state: TaskState::Fresh,
                percent: 0.0,
                source_language: None,
                fallback_count: None,
            });
        }
        Some(c) => c,
    };

    // 指纹感知：只比对可从当前 config 得知的 4 个字段（source_language 交给
    // 前端自动选回 + 按钮守卫，运行时管线做最终校验）。任一不同 → Fresh，
    // 不对用户承诺与实际不符的「继续处理 N%」
    let config = state.config.lock().await.clone();
    let fp = &cp.fingerprint;
    if fp.target_lang != config.translate.target_lang
        || fp.translate_provider != config.translate.provider
        || fp.translate_model != config.translate.model
        || fp.asr_model != config.asr.file_model
    {
        return Ok(TaskStatus {
            state: TaskState::Fresh,
            percent: 0.0,
            source_language: None,
            fallback_count: None,
        });
    }

    if cp.segments.is_empty() {
        Ok(TaskStatus {
            state: TaskState::Fresh,
            percent: 0.0,
            source_language: None,
            fallback_count: None,
        })
    } else if cp.is_all_completed() {
        let fallback_count = cp.segments.iter().filter(|s| s.fallback).count();
        Ok(TaskStatus {
            state: TaskState::Completed,
            percent: 1.0,
            source_language: Some(cp.fingerprint.source_language.clone()),
            fallback_count: (fallback_count > 0).then_some(fallback_count),
        })
    } else {
        Ok(TaskStatus {
            state: TaskState::Translating,
            percent: cp.percent(),
            source_language: Some(cp.fingerprint.source_language.clone()),
            fallback_count: None,
        })
    }
}

#[tauri::command]
pub async fn list_recent_tasks(app: tauri::AppHandle) -> Result<Vec<RecentTask>, String> {
    let tasks_dir = tasks_root(&app)?;

    if !tasks_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(&tasks_dir).map_err(|e| e.to_string())?;
    let mut tasks: Vec<RecentTask> = Vec::new();

    for entry in entries.flatten() {
        let progress_file = entry.path().join("progress.jsonl");
        let modified_at = match std::fs::metadata(&progress_file).and_then(|m| m.modified()) {
            Ok(t) => t
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Err(_) => continue,
        };
        let cp = match Checkpoint::load(&progress_file) {
            Ok(Some(c)) => c,
            Ok(None) => continue, // 无/已归档损坏文件
            Err(_) => continue,
        };
        if cp.video_path.as_os_str().is_empty() {
            continue;
        }
        let video_path = cp.video_path.to_string_lossy().to_string();
        let (file_name, task_type) = if video_path.starts_with("realtime://") {
            let display = video_path.strip_prefix("realtime://").unwrap_or(&video_path);
            (format!("实时会话 {}", display), "realtime".to_string())
        } else {
            let name = PathBuf::from(&video_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            (name, "file".to_string())
        };

        let (state, percent) = if cp.segments.is_empty() {
            (TaskState::Fresh, 0.0)
        } else if cp.is_all_completed() {
            (TaskState::Completed, 1.0)
        } else {
            (TaskState::Translating, cp.percent())
        };

        tasks.push(RecentTask {
            task_id: cp.task_id,
            video_path,
            file_name,
            modified_at,
            state,
            percent,
            task_type,
        });
    }

    // 同一视频多个 task_id（文件被替换过）只保留最新
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut deduped: Vec<RecentTask> = Vec::new();
    for t in tasks {
        match seen.get(&t.video_path) {
            Some(&i) => {
                if t.modified_at > deduped[i].modified_at {
                    deduped[i] = t;
                }
            }
            None => {
                seen.insert(t.video_path.clone(), deduped.len());
                deduped.push(t);
            }
        }
    }

    deduped.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    deduped.truncate(8);
    Ok(deduped)
}

/// 实时会话状态（单一 Mutex 包裹，避免多 Mutex 死锁）
pub struct RealtimeSessionInner {
    pub pipeline: Option<pick_up_sound_text::pipeline::realtime::RealtimePipeline>,
    pub session_task: Option<JoinHandle<()>>,
    pub cancel_token: Option<CancellationToken>,
    pub session_id: Option<String>,
    pub state: pick_up_sound_text::pipeline::realtime::RealtimeState,
}
