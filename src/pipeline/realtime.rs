//! Realtime pipeline: capture → ASR → translate (concurrent streaming).

use std::sync::Arc;
use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::asr::{AsrConfig, AsrEvent, AsrProvider, AsrStream};
use crate::audio::capture::CaptureSource;
use crate::audio::AudioChunk;
use crate::checkpoint::{Checkpoint, CheckpointFingerprint, SegmentProgress, SegmentStatus};
use crate::config::AppConfig;
use crate::error::Error;
use crate::subtitle::{SubtitleEntry, SubtitleStatus};
use crate::translate::{TranslateProvider, TranslateRequest};
use crate::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeState {
    Idle,
    Connecting,
    Listening,
    Paused,
    Reconnecting,
    Stopped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct PartialEntry {
    pub source: String,
    pub ts_start: f64,
    pub ts_end: f64,
}

/// 实时字幕管线：三并发 task 协作
pub struct RealtimePipeline {
    state: Arc<Mutex<RealtimeState>>,
    finalized_entries: Arc<Mutex<Vec<SubtitleEntry>>>,
    current_partial: Arc<Mutex<Option<PartialEntry>>>,
    config: AppConfig,
    source_language: String,
    session_id: String,
    capture_target_ids: Vec<String>,
    session_offset_ms: Arc<Mutex<i64>>,
    cancel: CancellationToken,
    capture_source: Option<Box<dyn CaptureSource>>,
    asr_provider: Option<Box<dyn AsrProvider>>,
    translate_provider: Option<Arc<dyn TranslateProvider>>,
    checkpoint_path: Option<PathBuf>,
    checkpoint: Option<Checkpoint>,
}

impl RealtimePipeline {
    pub fn new(
        capture_source: Box<dyn CaptureSource>,
        asr_provider: Box<dyn AsrProvider>,
        translate_provider: Arc<dyn TranslateProvider>,
        config: AppConfig,
        source_language: String,
        session_id: String,
        capture_target_ids: Vec<String>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(RealtimeState::Idle)),
            finalized_entries: Arc::new(Mutex::new(Vec::new())),
            current_partial: Arc::new(Mutex::new(None)),
            config,
            source_language,
            session_id,
            capture_target_ids,
            session_offset_ms: Arc::new(Mutex::new(0)),
            cancel: CancellationToken::new(),
            capture_source: Some(capture_source),
            asr_provider: Some(asr_provider),
            translate_provider: Some(translate_provider),
            checkpoint_path: None,
            checkpoint: None,
        }
    }

    /// 测试用构造器：不注入真实 provider
    pub fn new_for_test() -> Self {
        Self {
            state: Arc::new(Mutex::new(RealtimeState::Idle)),
            finalized_entries: Arc::new(Mutex::new(Vec::new())),
            current_partial: Arc::new(Mutex::new(None)),
            config: AppConfig::default(),
            source_language: "auto".to_string(),
            session_id: "test-session".to_string(),
            capture_target_ids: Vec::new(),
            session_offset_ms: Arc::new(Mutex::new(0)),
            cancel: CancellationToken::new(),
            capture_source: None,
            asr_provider: None,
            translate_provider: None,
            checkpoint_path: None,
            checkpoint: None,
        }
    }

    pub async fn get_state(&self) -> RealtimeState {
        self.state.lock().await.clone()
    }

    pub fn state_handle(&self) -> Arc<Mutex<RealtimeState>> {
        self.state.clone()
    }

    pub fn entries_handle(&self) -> Arc<Mutex<Vec<SubtitleEntry>>> {
        self.finalized_entries.clone()
    }

    pub fn partial_handle(&self) -> Arc<Mutex<Option<PartialEntry>>> {
        self.current_partial.clone()
    }

    pub fn cancel_handle(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 初始化 checkpoint（目录可注入）
    pub fn init_checkpoint_in(
        &mut self,
        dir: PathBuf,
        fingerprint: CheckpointFingerprint,
    ) -> Result<()> {
        let checkpoint_dir = dir.join(&self.session_id);
        std::fs::create_dir_all(&checkpoint_dir)?;
        let checkpoint_path = checkpoint_dir.join("progress.jsonl");

        let video_path = PathBuf::from(format!("realtime://{}", self.session_id));
        let checkpoint = match Checkpoint::load(&checkpoint_path)? {
            Some(c) => c,
            None => Checkpoint::new(self.session_id.clone(), video_path, fingerprint),
        };
        self.checkpoint = Some(checkpoint);
        self.checkpoint_path = Some(checkpoint_path);
        Ok(())
    }

    /// 启动三并发 task（capture → ASR → translate）
    pub async fn start(&mut self, app_handle: tauri::AppHandle) -> Result<()> {
        let mut state = self.state.lock().await;
        if *state != RealtimeState::Idle && *state != RealtimeState::Stopped {
            return Err(Error::AudioSource("Session already running".to_string()));
        }
        *state = RealtimeState::Connecting;
        drop(state);

        // 启动 ASR 连接
        let asr_config = AsrConfig {
            provider: self.config.asr.provider.clone(),
            model: self.config.asr.realtime_model.clone(),
            api_key: self.config.asr.api_key.clone(),
            language: self.source_language.clone(),
            workspace_id: self.config.asr.workspace_id.clone(),
        };

        let asr_provider = self.asr_provider.as_ref()
            .ok_or_else(|| Error::Asr("ASR provider not initialized".to_string()))?;
        let asr_stream = asr_provider.start_stream(&asr_config).await?;

        // 启动音频采集（用户选中的音源；空则由平台实现决定默认行为）
        let capture_source = self.capture_source.as_mut()
            .ok_or_else(|| Error::AudioSource("Capture source not initialized".to_string()))?;
        let audio_rx = capture_source.start(&self.capture_target_ids.clone()).await?;

        // 更新状态
        let mut state = self.state.lock().await;
        *state = RealtimeState::Listening;
        drop(state);

        // 发射状态变更事件
        let _ = app_handle.emit("realtime:state-change", RealtimeStateEvent::new(
            RealtimeState::Listening,
            Some(self.session_id.clone()),
        ));

        // 启动三并发 task
        let (asr_audio_tx, asr_audio_rx) = mpsc::channel::<Vec<i16>>(100);
        let (translate_tx, translate_rx) = mpsc::channel::<TranslateJob>(100);

        let capture_task = self.spawn_capture_task(audio_rx, asr_audio_tx);
        let asr_task = self.spawn_asr_task(asr_stream, asr_audio_rx, translate_tx, app_handle.clone());
        let translate_task = self.spawn_translate_task(translate_rx, app_handle.clone());

        // 等待所有 task 完成（或被取消）
        tokio::select! {
            _ = self.cancel.cancelled() => {
                tracing::info!("Realtime pipeline cancelled");
            }
            _ = capture_task => {
                tracing::info!("Capture task ended");
            }
            _ = asr_task => {
                tracing::info!("ASR task ended");
            }
            _ = translate_task => {
                tracing::info!("Translate task ended");
            }
        }

        Ok(())
    }

    /// 暂停：停止采集 + 关闭 ASR 连接
    pub async fn pause(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        if *state != RealtimeState::Listening {
            return Err(Error::AudioSource("Not listening".to_string()));
        }
        *state = RealtimeState::Paused;
        drop(state);

        // 触发取消（capture task 停止转发，ASR task 发送 finish-task）
        self.cancel.cancel();

        // 记录当前会话偏移量（恢复时用于时间戳校正）
        let entries = self.finalized_entries.lock().await;
        if let Some(last) = entries.last() {
            let mut offset = self.session_offset_ms.lock().await;
            *offset = (last.content_end * 1000.0) as i64;
        }
        drop(entries);

        Ok(())
    }

    /// 恢复：新建 ASR 连接，继续采集
    pub async fn resume(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        if *state != RealtimeState::Paused {
            return Err(Error::AudioSource("Not paused".to_string()));
        }
        *state = RealtimeState::Reconnecting;
        drop(state);

        // 重置 cancel token（新建 ASR 连接）
        self.cancel = CancellationToken::new();

        // 重建 ASR 连接
        let asr_config = AsrConfig {
            provider: self.config.asr.provider.clone(),
            model: self.config.asr.realtime_model.clone(),
            api_key: self.config.asr.api_key.clone(),
            language: self.source_language.clone(),
            workspace_id: self.config.asr.workspace_id.clone(),
        };

        let asr_provider = self.asr_provider.as_ref()
            .ok_or_else(|| Error::Asr("ASR provider not initialized".to_string()))?;
        let _asr_stream = asr_provider.start_stream(&asr_config).await?;

        // TODO: 恢复采集（需要重新启动 capture source）
        // 当前简化：状态直接回 Listening，实际音频采集需要 capture_source 支持 restart

        let mut state = self.state.lock().await;
        *state = RealtimeState::Listening;
        drop(state);

        Ok(())
    }

    /// 停止：采集停止 → ASR finish → 翻译 drain → checkpoint save
    pub async fn stop(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        if *state == RealtimeState::Stopped {
            return Ok(());
        }
        *state = RealtimeState::Stopped;
        drop(state);

        // 触发取消
        self.cancel.cancel();

        // 保存 checkpoint
        if let (Some(cp), Some(cp_path)) = (&self.checkpoint, &self.checkpoint_path) {
            if let Err(e) = cp.save(cp_path) {
                tracing::warn!("checkpoint 终态保存失败: {}", e);
            }
        }

        Ok(())
    }

    fn spawn_capture_task(
        &self,
        mut audio_rx: mpsc::Receiver<AudioChunk>,
        asr_audio_tx: mpsc::Sender<Vec<i16>>,
    ) -> JoinHandle<()> {
        let cancel = self.cancel.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("Capture task cancelled");
                        break;
                    }
                    chunk = audio_rx.recv() => {
                        match chunk {
                            Some(audio_chunk) => {
                                if asr_audio_tx.send(audio_chunk.pcm).await.is_err() {
                                    tracing::warn!("ASR audio channel closed");
                                    break;
                                }
                            }
                            None => {
                                tracing::info!("Audio source exhausted");
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    fn spawn_asr_task(
        &self,
        mut asr_stream: Box<dyn AsrStream>,
        mut asr_audio_rx: mpsc::Receiver<Vec<i16>>,
        translate_tx: mpsc::Sender<TranslateJob>,
        app_handle: tauri::AppHandle,
    ) -> JoinHandle<()> {
        let cancel = self.cancel.clone();
        let state = self.state.clone();
        let finalized_entries = self.finalized_entries.clone();
        let current_partial = self.current_partial.clone();
        let session_offset_ms = self.session_offset_ms.clone();
        let checkpoint_path = self.checkpoint_path.clone();

        tokio::spawn(async move {
            let mut entry_index = 0usize;
            let mut last_partial_emit = std::time::Instant::now();

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("ASR task cancelled, sending finish-task");
                        let _ = asr_stream.finish().await;
                        break;
                    }
                    // 发送音频数据到 ASR
                    pcm = asr_audio_rx.recv() => {
                        match pcm {
                            Some(samples) => {
                                if let Err(e) = asr_stream.send_audio(&samples).await {
                                    tracing::error!("Failed to send audio to ASR: {}", e);
                                    let mut state = state.lock().await;
                                    *state = RealtimeState::Failed;
                                    drop(state);
                                    let _ = app_handle.emit("realtime:state-change",
                                        RealtimeStateEvent::failed(format!("ASR send error: {}", e), None));
                                    break;
                                }
                            }
                            None => {
                                tracing::info!("Audio channel closed, finishing ASR");
                                let _ = asr_stream.finish().await;
                            }
                        }
                    }
                    // 接收 ASR 事件
                    event = asr_stream.next_event() => {
                        match event {
                            Ok(AsrEvent::Partial { text, ts_start, ts_end }) => {
                                let offset = *session_offset_ms.lock().await;
                                let mut partial = current_partial.lock().await;
                                *partial = Some(PartialEntry {
                                    source: text.clone(),
                                    ts_start: ts_start + offset as f64 / 1000.0,
                                    ts_end: ts_end + offset as f64 / 1000.0,
                                });
                                drop(partial);

                                // 节流 emit（200ms）
                                if last_partial_emit.elapsed().as_millis() >= 200 {
                                    let _ = app_handle.emit("realtime:subtitle-partial", SubtitlePartialEvent {
                                        entry_index,
                                        source: text,
                                        ts_start,
                                        ts_end,
                                    });
                                    last_partial_emit = std::time::Instant::now();
                                }
                            }
                            Ok(AsrEvent::Final { text, ts_start, ts_end }) => {
                                let offset = *session_offset_ms.lock().await;
                                let adjusted_start = ts_start + offset as f64 / 1000.0;
                                let adjusted_end = ts_end + offset as f64 / 1000.0;

                                let entry = SubtitleEntry {
                                    content_start: adjusted_start,
                                    content_end: adjusted_end,
                                    wall_start: (adjusted_start * 1000.0) as i64,
                                    wall_end: (adjusted_end * 1000.0) as i64,
                                    source: text.clone(),
                                    translated: String::new(),
                                    status: SubtitleStatus::Final,
                                };

                                let mut entries = finalized_entries.lock().await;
                                entries.push(entry.clone());
                                drop(entries);

                                // 清空 current_partial
                                let mut partial = current_partial.lock().await;
                                *partial = None;
                                drop(partial);

                                // emit final 事件
                                let _ = app_handle.emit("realtime:subtitle-final", SubtitleFinalEvent {
                                    entry_index,
                                    entry: entry.clone(),
                                });

                                // 送入翻译队列
                                let _ = translate_tx.send(TranslateJob {
                                    entry_index,
                                    source: text.clone(),
                                }).await;

                                // checkpoint append
                                if let Some(cp_path) = &checkpoint_path {
                                    let seg = SegmentProgress {
                                        segment_id: entry_index,
                                        start_time: Some(adjusted_start),
                                        end_time: Some(adjusted_end),
                                        status: SegmentStatus::Pending,
                                        source: Some(text),
                                        translated: None,
                                        fallback: false,
                                    };
                                    let _ = Checkpoint::append_updates(cp_path, &[seg]);
                                }

                                entry_index += 1;
                            }
                            Ok(AsrEvent::Error { code, message }) => {
                                tracing::error!("ASR error: {} - {}", code, message);
                                let mut state = state.lock().await;
                                *state = RealtimeState::Failed;
                                drop(state);
                                let _ = app_handle.emit("realtime:state-change",
                                    RealtimeStateEvent::failed(format!("ASR error: {}", message), None));
                                break;
                            }
                            Ok(AsrEvent::EndOfStream) => {
                                tracing::info!("ASR stream ended");
                                break;
                            }
                            Err(e) => {
                                tracing::error!("ASR stream error: {}", e);
                                let mut state = state.lock().await;
                                *state = RealtimeState::Failed;
                                drop(state);
                                let _ = app_handle.emit("realtime:state-change",
                                    RealtimeStateEvent::failed(e.to_string(), None));
                                break;
                            }
                        }
                    }
                }
            }
        })
    }

    fn spawn_translate_task(
        &self,
        mut translate_rx: mpsc::Receiver<TranslateJob>,
        app_handle: tauri::AppHandle,
    ) -> JoinHandle<()> {
        let cancel = self.cancel.clone();
        let finalized_entries = self.finalized_entries.clone();
        let translate_provider = self.translate_provider.clone()
            .expect("translate provider must be initialized");
        let target_lang = self.config.translate.target_lang.clone();
        let source_language = self.source_language.clone();
        let checkpoint_path = self.checkpoint_path.clone();

        tokio::spawn(async move {
            let mut previous_texts: Vec<String> = Vec::new();

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("Translate task cancelled");
                        break;
                    }
                    job = translate_rx.recv() => {
                        match job {
                            Some(TranslateJob { entry_index, source }) => {
                                let req = TranslateRequest {
                                    text: source.clone(),
                                    source_lang: source_language.clone(),
                                    target_lang: target_lang.clone(),
                                    context: previous_texts.iter().rev().take(10).cloned().collect(),
                                    glossary: None,
                                };

                                match translate_provider.translate(req).await {
                                    Ok(resp) => {
                                        let mut entries = finalized_entries.lock().await;
                                        if let Some(entry) = entries.get_mut(entry_index) {
                                            entry.translated = resp.translated_text.clone();
                                        }
                                        drop(entries);

                                        let _ = app_handle.emit("realtime:translation", TranslationEvent {
                                            entry_index,
                                            translated: resp.translated_text.clone(),
                                        });

                                        if let Some(cp_path) = &checkpoint_path {
                                            let seg = SegmentProgress {
                                                segment_id: entry_index,
                                                start_time: None,
                                                end_time: None,
                                                status: SegmentStatus::Completed,
                                                source: None,
                                                translated: Some(resp.translated_text),
                                                fallback: false,
                                            };
                                            let _ = Checkpoint::append_updates(cp_path, &[seg]);
                                        }

                                        previous_texts.push(source);
                                    }
                                    Err(e) => {
                                        tracing::warn!("Translation failed for entry {}: {}", entry_index, e);
                                        let mut entries = finalized_entries.lock().await;
                                        if let Some(entry) = entries.get_mut(entry_index) {
                                            entry.translated = "[翻译失败]".to_string();
                                        }
                                        drop(entries);
                                    }
                                }
                            }
                            None => {
                                tracing::info!("Translate queue closed");
                                break;
                            }
                        }
                    }
                }
            }
        })
    }
}

/// 翻译任务
#[derive(Debug)]
struct TranslateJob {
    entry_index: usize,
    source: String,
}

/// 事件 payload
#[derive(Clone, Serialize)]
pub struct SubtitlePartialEvent {
    pub entry_index: usize,
    pub source: String,
    pub ts_start: f64,
    pub ts_end: f64,
}

#[derive(Clone, Serialize)]
pub struct SubtitleFinalEvent {
    pub entry_index: usize,
    pub entry: SubtitleEntry,
}

#[derive(Clone, Serialize)]
pub struct TranslationEvent {
    pub entry_index: usize,
    pub translated: String,
}

#[derive(Clone, Serialize)]
pub struct RealtimeStateEvent {
    pub state: RealtimeState,
    pub session_id: Option<String>,
    /// 进入 failed 态时的错误详情，供前端直接展示
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RealtimeStateEvent {
    pub fn new(state: RealtimeState, session_id: Option<String>) -> Self {
        Self { state, session_id, error: None }
    }

    pub fn failed(message: String, session_id: Option<String>) -> Self {
        Self {
            state: RealtimeState::Failed,
            session_id,
            error: Some(message),
        }
    }
}
