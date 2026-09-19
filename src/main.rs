// Windows 发布版隐藏控制台窗口（否则双击启动会挂一个日志黑窗）；
// debug 构建保留控制台便于看 tracing 输出
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use commands::{
    delete_task, export_subtitle, get_config, get_processing_progress, get_task_status,
    list_recent_tasks, pause_file_processing, save_config, start_file_processing,
    stop_file_processing, test_asr_connection, test_translate_connection, AppState,
    RealtimeSessionInner,
};
use std::sync::Arc;
use tauri::Manager;
#[cfg(target_os = "macos")]
use tauri::Emitter;
#[cfg(target_os = "macos")]
use tauri::menu::{Menu, SubmenuBuilder};
use tokio::sync::Mutex;

/// 原生菜单栏（仅 macOS）。「关于拾言」走自定义菜单项发事件给前端弹 About——
/// 原生 About 面板在 macOS 只显示版本/版权，放不下 GitHub/邮箱等署名信息。
/// Windows 无菜单栏：About 入口在设置抽屉（前端按平台渲染）
#[cfg(target_os = "macos")]
fn app_menu(handle: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let app_submenu = SubmenuBuilder::new(handle, "拾言")
        .text("about", "关于拾言")
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let edit_submenu = SubmenuBuilder::new(handle, "编辑")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let window_submenu = SubmenuBuilder::new(handle, "窗口")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;

    Menu::with_items(handle, &[&app_submenu, &edit_submenu, &window_submenu])
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"))
        )
        .init();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::Manager;
            // setup 错误类型以 Tauri 签名为准：Box<dyn std::error::Error>
            let data_dir = app.path().app_data_dir().map_err(
                |e| -> Box<dyn std::error::Error> { format!("获取应用数据目录失败: {e}").into() },
            )?;
            std::fs::create_dir_all(&data_dir).map_err(
                |e| -> Box<dyn std::error::Error> { format!("创建应用数据目录失败: {e}").into() },
            )?;
            // 先迁移（旧 mac 数据），再加载——顺序不可反：迁移必须先于任何
            // KeyStore::new(data_dir)，否则新位置先生成新盐，旧密文永不可解
            if let Some(legacy) = pick_up_sound_text::config::legacy_config_dir() {
                if let Err(e) =
                    pick_up_sound_text::config::migrate_legacy_config(&legacy, &data_dir)
                {
                    tracing::warn!("旧配置迁移失败（不影响启动）: {e}");
                }
            }
            let config = match pick_up_sound_text::config::AppConfig::load(&data_dir) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        "配置加载失败，使用默认配置（若此前已保存密钥，密文可能无法解密）: {e}"
                    );
                    Default::default()
                }
            };

            let app_state = AppState {
                pipeline_state: Arc::new(Mutex::new(None)),
                pipeline_entries: Arc::new(Mutex::new(None)),
                processing_task: Arc::new(Mutex::new(None)),
                config: Arc::new(Mutex::new(config)),
                data_dir,
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
            app.manage(app_state);
            Ok(())
        });

    // 菜单栏仅 macOS；Windows 原生标题栏 + 设置抽屉内 About 入口，不设菜单
    #[cfg(target_os = "macos")]
    let builder = builder.menu(|handle| app_menu(handle)).on_menu_event(|app, event| {
        if event.id().0 == "about" {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.emit("open-about", ());
            }
        }
    });

    builder
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

                    // 清理完毕，允许关闭。必须用 destroy() 而非 close()：
                    // close() 会再次触发 CloseRequested → 本处理器再次
                    // prevent_close + spawn → 无限递归，事件循环刷爆、
                    // CPU 打满、窗口冻结永不退出（真机 B4 验收发现）
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.destroy();
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
            commands::get_host_platform,
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
