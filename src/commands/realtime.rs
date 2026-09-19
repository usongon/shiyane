use pick_up_sound_text::asr::DashScopeAsrProvider;
use pick_up_sound_text::audio::capture::create_capture_source;
use pick_up_sound_text::checkpoint::CheckpointFingerprint;
use pick_up_sound_text::config::translate_provider_preset;
use pick_up_sound_text::pipeline::realtime::{RealtimePipeline, RealtimeState};
use pick_up_sound_text::translate::OpenAiCompatibleProvider;
use serde::Serialize;
use std::sync::Arc;
use tauri::{Manager, State};

use super::AppState;

#[derive(Serialize)]
pub struct RealtimeStateInfo {
    pub state: String,
    pub session_id: Option<String>,
    pub entry_count: usize,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct CaptureTargetInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub icon_path: Option<String>,
}

#[tauri::command]
pub async fn start_realtime_session(
    source_language: String,
    capture_target_ids: Vec<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    tracing::info!("start_realtime_session: language={}, targets={:?}", source_language, capture_target_ids);

    // 检查是否已有活跃会话（failed 也允许重启——失败后点「新建会话」是主路径）。
    // 优先读 pipeline 内部状态：inner.state 是命令层的镜像，ASR 中途失败只反映在 pipeline 里
    let realtime = state.realtime.lock().await;
    let current = if let Some(pipeline) = &realtime.pipeline {
        pipeline.get_state().await
    } else {
        realtime.state.clone()
    };
    if current != RealtimeState::Idle
        && current != RealtimeState::Stopped
        && current != RealtimeState::Failed
    {
        return Err("已有活跃的实时会话".to_string());
    }
    drop(realtime);

    let config = state.config.lock().await.clone();

    // 生成 session_id
    let session_id = format!("realtime-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));

    // 创建 providers
    let mut capture_source = create_capture_source();
    let asr_provider = DashScopeAsrProvider;
    let (base_url, _) = translate_provider_preset(&config.translate.provider);
    let translate_provider = Arc::new(OpenAiCompatibleProvider {
        base_url: base_url.to_string(),
        model: config.translate.model.clone(),
        api_key: config.translate.api_key.clone(),
        timeout_secs: 60,
    });

    // 先测试音频采集是否可用（快速失败，不进入后台 task）
    if let Err(e) = capture_source.start(&capture_target_ids).await {
        return Err(format!("音频采集启动失败：{}", e));
    }
    // 测试成功后停止，等 pipeline 正式启动时再重新 start
    let _ = capture_source.stop().await;

    // 创建 pipeline
    let mut pipeline = RealtimePipeline::new(
        capture_source,
        Box::new(asr_provider),
        translate_provider,
        config.clone(),
        source_language.clone(),
        session_id.clone(),
        capture_target_ids.clone(),
    );

    // 初始化 checkpoint
    let app_data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("tasks");
    let fingerprint = CheckpointFingerprint {
        source_language: source_language.clone(),
        target_lang: config.translate.target_lang.clone(),
        translate_provider: config.translate.provider.clone(),
        translate_model: config.translate.model.clone(),
        asr_model: config.asr.realtime_model.clone(),
    };
    pipeline.init_checkpoint_in(app_data_dir, fingerprint).map_err(|e| e.to_string())?;

    // 存储到 AppState
    let cancel_token = pipeline.cancel_handle();
    let mut realtime = state.realtime.lock().await;
    realtime.pipeline = Some(pipeline);
    realtime.cancel_token = Some(cancel_token);
    realtime.session_id = Some(session_id.clone());
    realtime.state = RealtimeState::Connecting;
    realtime.last_error = None;
    drop(realtime);

    // 启动 pipeline（在后台 task 中）；失败必须 emit 事件——invoke 已返回，
    // 前端只能靠 state-change 事件拿到失败原因并展示
    let realtime_arc = state.realtime.clone();
    let app_handle = app.clone();
    let session_id_for_err = session_id.clone();
    let session_task = tokio::spawn(async move {
        let mut realtime = realtime_arc.lock().await;
        if let Some(pipeline) = realtime.pipeline.as_mut() {
            if let Err(e) = pipeline.start(app_handle.clone()).await {
                tracing::error!("Realtime pipeline error: {}", e);
                realtime.state = RealtimeState::Failed;
                realtime.last_error = Some(e.to_string());
                use tauri::Emitter;
                let _ = app_handle.emit(
                    "realtime:state-change",
                    pick_up_sound_text::pipeline::realtime::RealtimeStateEvent::failed(
                        e.to_string(),
                        Some(session_id_for_err),
                    ),
                );
            }
        }
    });

    let mut realtime = state.realtime.lock().await;
    realtime.session_task = Some(session_task);
    drop(realtime);

    Ok(session_id)
}

#[tauri::command]
pub async fn pause_realtime_session(state: State<'_, AppState>) -> Result<(), String> {
    let mut realtime = state.realtime.lock().await;
    if let Some(pipeline) = realtime.pipeline.as_mut() {
        pipeline.pause().await.map_err(|e| e.to_string())?;
        realtime.state = RealtimeState::Paused;
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_realtime_session(state: State<'_, AppState>) -> Result<(), String> {
    let mut realtime = state.realtime.lock().await;
    if let Some(pipeline) = realtime.pipeline.as_mut() {
        if let Err(e) = pipeline.resume().await {
            // 镜像同步 pipeline 实际终态：连接失败为 Failed（可新建会话），
            // 前置校验失败仍为 Paused（可重试/停止）；失败原因经 invoke 返回前端
            realtime.state = pipeline.get_state().await;
            realtime.last_error = Some(e.to_string());
            return Err(e.to_string());
        }
        realtime.state = RealtimeState::Listening;
    }
    Ok(())
}

#[tauri::command]
pub async fn stop_realtime_session(state: State<'_, AppState>) -> Result<(), String> {
    let mut realtime = state.realtime.lock().await;
    if let Some(pipeline) = realtime.pipeline.as_mut() {
        pipeline.stop().await.map_err(|e| e.to_string())?;
        realtime.state = RealtimeState::Stopped;
    }
    // 等待 session task 结束
    if let Some(task) = realtime.session_task.take() {
        let _ = task.await;
    }
    Ok(())
}

/// 呼出/收起悬浮字幕窗（subtitle-overlay），返回操作后的可见状态
#[tauri::command]
pub fn toggle_realtime_overlay(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri::Manager;
    let win = app
        .get_webview_window("subtitle-overlay")
        .ok_or_else(|| "悬浮字幕窗口未创建".to_string())?;
    let visible = win.is_visible().map_err(|e| e.to_string())?;
    if visible {
        win.hide().map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[tauri::command]
pub async fn list_capture_targets(_state: State<'_, AppState>) -> Result<Vec<CaptureTargetInfo>, String> {
    let capture_source = create_capture_source();
    let targets = capture_source.list_targets().await.map_err(|e| e.to_string())?;
    Ok(targets.into_iter().map(|t| CaptureTargetInfo {
        id: t.id,
        name: t.name,
        kind: match t.kind {
            pick_up_sound_text::audio::capture::CaptureKind::SystemAudio => "system_audio".to_string(),
            pick_up_sound_text::audio::capture::CaptureKind::Microphone => "microphone".to_string(),
        },
        icon_path: t.icon_path,
    }).collect())
}

#[tauri::command]
pub async fn get_realtime_state(state: State<'_, AppState>) -> Result<RealtimeStateInfo, String> {
    let realtime = state.realtime.lock().await;
    // pipeline 存在时读其内部状态（真实态）；否则退回命令层镜像（如启动前的 connecting）
    let current = if let Some(pipeline) = &realtime.pipeline {
        pipeline.get_state().await
    } else {
        realtime.state.clone()
    };
    let state_str = match &current {
        RealtimeState::Idle => "idle",
        RealtimeState::Connecting => "connecting",
        RealtimeState::Listening => "listening",
        RealtimeState::Paused => "paused",
        RealtimeState::Reconnecting => "reconnecting",
        RealtimeState::Stopped => "stopped",
        RealtimeState::Failed => "failed",
    };
    let error = if current == RealtimeState::Failed {
        realtime.last_error.clone()
    } else {
        None
    };

    let entry_count = if let Some(pipeline) = &realtime.pipeline {
        pipeline.entries_handle().lock().await.len()
    } else {
        0
    };

    Ok(RealtimeStateInfo {
        state: state_str.to_string(),
        session_id: realtime.session_id.clone(),
        entry_count,
        error,
    })
}
