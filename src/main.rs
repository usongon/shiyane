mod commands;

use commands::{
    delete_task, export_subtitle, get_config, get_processing_progress, get_task_status,
    list_recent_tasks, pause_file_processing, save_config, start_file_processing,
    stop_file_processing, test_asr_connection, test_translate_connection, AppState,
    RealtimeSessionInner,
};
use pick_up_sound_text::config::AppConfig;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"))
        )
        .init();

    let config = AppConfig::load().unwrap_or_default();

    let app_state = AppState {
        pipeline_state: Arc::new(Mutex::new(None)),
        pipeline_entries: Arc::new(Mutex::new(None)),
        processing_task: Arc::new(Mutex::new(None)),
        config: Arc::new(Mutex::new(config)),
        pipeline: Arc::new(Mutex::new(None)),
        pipeline_progress: Arc::new(Mutex::new(None)),
        pipeline_phase: Arc::new(Mutex::new(None)),
        cancel_token: Arc::new(Mutex::new(None)),
        running_video_path: Arc::new(Mutex::new(None)),
        running_task_id: Arc::new(Mutex::new(None)),
        control: Arc::new(Mutex::new(())),
        realtime: Arc::new(Mutex::new(RealtimeSessionInner {
            pipeline: None,
            session_task: None,
            cancel_token: None,
            session_id: None,
            state: pick_up_sound_text::pipeline::realtime::RealtimeState::Idle,
            last_error: None,
        })),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 只处理主窗口
                if window.label() != "main" {
                    return;
                }

                let app_handle = window.app_handle().clone();

                // 先阻止关闭，在 async task 中检查并清理
                api.prevent_close();

                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<AppState>();
                    let mut realtime = state.realtime.lock().await;

                    let has_active = matches!(
                        realtime.state,
                        pick_up_sound_text::pipeline::realtime::RealtimeState::Listening
                            | pick_up_sound_text::pipeline::realtime::RealtimeState::Paused
                            | pick_up_sound_text::pipeline::realtime::RealtimeState::Connecting
                    );

                    if has_active {
                        tracing::info!("Window closing: stopping realtime session");
                        if let Some(pipeline) = realtime.pipeline.as_mut() {
                            let _ = pipeline.stop().await;
                        }
                        if let Some(task) = realtime.session_task.take() {
                            let _ = task.await;
                        }
                    }

                    drop(realtime);

                    // 主窗关闭时一并收起悬浮字幕窗，否则进程会因仍有窗口而驻留
                    if let Some(overlay) = app_handle.get_webview_window("subtitle-overlay") {
                        let _ = overlay.hide();
                    }

                    // 清理完毕，允许关闭
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.close();
                    }
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_file_processing,
            pause_file_processing,
            stop_file_processing,
            delete_task,
            get_processing_progress,
            export_subtitle,
            get_config,
            save_config,
            test_asr_connection,
            test_translate_connection,
            list_recent_tasks,
            get_task_status,
            commands::realtime::start_realtime_session,
            commands::realtime::pause_realtime_session,
            commands::realtime::resume_realtime_session,
            commands::realtime::stop_realtime_session,
            commands::realtime::list_capture_targets,
            commands::realtime::get_realtime_state,
            commands::realtime::toggle_realtime_overlay,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
