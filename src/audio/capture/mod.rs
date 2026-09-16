pub mod macos;
pub mod windows;

use crate::audio::AudioChunk;
use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub id: String,
    pub name: String,
    pub kind: CaptureKind,
    pub icon_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    SystemAudio,
    Microphone,
}

#[async_trait]
pub trait CaptureSource: Send + Sync {
    async fn start(&mut self, target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>>;
    async fn stop(&mut self) -> Result<()>;
    async fn list_targets(&self) -> Result<Vec<CaptureTarget>>;
}

pub fn create_capture_source() -> Box<dyn CaptureSource> {
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacOSCaptureSource::new());
    #[cfg(target_os = "windows")]
    return Box::new(windows::WindowsCaptureSource::new());
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    compile_error!("Unsupported platform for audio capture");
}
