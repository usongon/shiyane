pub mod dashscope;
pub mod dashscope_filetrans;

pub use dashscope::DashScopeAsrProvider;
pub use dashscope_filetrans::DashScopeFileTransProvider;

use crate::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq)]
pub enum AsrEvent {
    Partial {
        text: String,
        ts_start: f64,
        ts_end: f64,
    },
    Final {
        text: String,
        ts_start: f64,
        ts_end: f64,
    },
    Error {
        code: String,
        message: String,
    },
    EndOfStream,
}

#[derive(Debug, Clone)]
pub struct AsrConfig {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub language: String,
    pub workspace_id: Option<String>,
    /// 文件转写：开启说话人分离（实时链路忽略）
    pub diarization: bool,
}

#[async_trait]
pub trait AsrProvider: Send + Sync {
    async fn start_stream(&self, config: &AsrConfig) -> Result<Box<dyn AsrStream>>;
}

#[async_trait]
pub trait AsrStream: Send + Sync {
    async fn send_audio(&mut self, pcm: &[i16]) -> Result<()>;
    async fn next_event(&mut self) -> Result<AsrEvent>;
    /// Signal that no more audio will be sent. Implementations should
    /// notify the server (e.g. DashScope `finish-task`) so it can emit
    /// final results and close the task.
    async fn finish(&mut self) -> Result<()>;
}

/// Result from asynchronous file transcription
#[derive(Debug, Clone)]
pub struct FileTranscriptionResult {
    pub sentences: Vec<TranscriptionSentence>,
}

#[derive(Debug, Clone)]
pub struct TranscriptionSentence {
    pub text: String,
    pub begin_time: f64,  // seconds
    pub end_time: f64,    // seconds
}

/// Provider for asynchronous file transcription (submit → poll → download)
#[async_trait]
pub trait FileAsrProvider: Send + Sync {
    /// Transcribe a complete audio file and return all sentences with timestamps
    async fn transcribe_file(&self, config: &AsrConfig, audio_path: &std::path::Path) -> Result<FileTranscriptionResult>;
}
