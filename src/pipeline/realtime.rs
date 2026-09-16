//! Realtime pipeline: capture → ASR → translate (concurrent streaming).

use std::sync::Arc;
use serde::Serialize;
use tauri::Manager;
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
    Failed(String),
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
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(RealtimeState::Idle)),
            finalized_entries: Arc::new(Mutex::new(Vec::new())),
            current_partial: Arc::new(Mutex::new(None)),
            config,
            source_language,
            session_id,
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
        todo!("Task 3: start realtime pipeline")
    }

    /// 暂停：停止采集 + 关闭 ASR 连接
    pub async fn pause(&mut self) -> Result<()> {
        todo!("Task 3: pause realtime pipeline")
    }

    /// 恢复：新建 ASR 连接，继续采集
    pub async fn resume(&mut self) -> Result<()> {
        todo!("Task 3: resume realtime pipeline")
    }

    /// 停止：采集停止 → ASR finish → 翻译 drain → checkpoint save
    pub async fn stop(&mut self) -> Result<()> {
        todo!("Task 3: stop realtime pipeline")
    }
}
