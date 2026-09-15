use pick_up_sound_text::asr::DashScopeFileTransProvider;
use pick_up_sound_text::audio::FileAudioSource;
use pick_up_sound_text::checkpoint::{compute_task_id, Checkpoint, CheckpointFingerprint};
use pick_up_sound_text::config::{AppConfig, translate_provider_preset};
use pick_up_sound_text::pipeline::{FilePipeline, PipelineState};
use pick_up_sound_text::subtitle::{generate_srt, generate_vtt, SubtitleEntry};
use pick_up_sound_text::translate::{OpenAiCompatibleProvider, TranslateProvider};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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
}

#[derive(Serialize)]
pub struct RecentTask {
    pub task_id: String,
    pub video_path: String,
    pub file_name: String,
    pub modified_at: u64,
    pub state: TaskState,
    pub percent: f64,
}

pub struct AppState {
    pub pipeline_state: Arc<Mutex<Option<Arc<Mutex<PipelineState>>>>>,
    pub pipeline_entries: Arc<Mutex<Option<Arc<Mutex<Vec<SubtitleEntry>>>>>>,
    pub processing_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub config: Arc<Mutex<AppConfig>>,
    pub pipeline: Arc<Mutex<Option<FilePipeline>>>,
    pub pipeline_progress: Arc<Mutex<Option<Arc<Mutex<f64>>>>>,
    pub pipeline_phase: Arc<Mutex<Option<Arc<Mutex<pick_up_sound_text::pipeline::Phase>>>>>,
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
    tracing::info!("start_file_processing called: video_path={}, source_language={}", video_path, source_language);
    
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
        timeout_secs: 60,
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

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tasks");

    let fingerprint = CheckpointFingerprint {
        source_language: source_language.clone(),
        target_lang: config.translate.target_lang.clone(),
        translate_provider: config.translate.provider.clone(),
        translate_model: config.translate.model.clone(),
        asr_model: config.asr.file_model.clone(),
    };
    pipeline
        .init_checkpoint_in(app_data_dir, task_id.clone(), video_path.clone(), fingerprint)
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

    let file_path = app.dialog()
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
    if oss.endpoint.is_empty() || oss.bucket.is_empty() || oss.access_key_id.is_empty() || oss.access_key_secret.is_empty() {
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
) -> Result<TaskStatus, String> {
    let path = PathBuf::from(video_path);
    let task_id = compute_task_id(&path).map_err(|e| e.to_string())?;
    let cp_path = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tasks")
        .join(&task_id)
        .join("progress.jsonl");

    let cp = match Checkpoint::load(&cp_path).map_err(|e| e.to_string())? {
        None => return Ok(TaskStatus { state: TaskState::Fresh, percent: 0.0 }),
        Some(c) => c,
    };
    if cp.segments.is_empty() {
        Ok(TaskStatus { state: TaskState::Fresh, percent: 0.0 })
    } else if cp.is_all_completed() {
        Ok(TaskStatus { state: TaskState::Completed, percent: 1.0 })
    } else {
        Ok(TaskStatus { state: TaskState::Translating, percent: cp.percent() })
    }
}

#[tauri::command]
pub async fn list_recent_tasks(app: tauri::AppHandle) -> Result<Vec<RecentTask>, String> {
    let tasks_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("tasks");

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
        let file_name = PathBuf::from(&video_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

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
