use super::{CaptureSource, CaptureTarget};
use crate::audio::AudioChunk;
use crate::{Error, Result};
use async_trait::async_trait;
use tokio::sync::mpsc;

pub struct MacOSCaptureSource;

impl MacOSCaptureSource {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CaptureSource for MacOSCaptureSource {
    async fn start(&mut self, _target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>> {
        Err(Error::AudioSource("macOS audio capture not yet implemented".to_string()))
    }

    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    async fn list_targets(&self) -> Result<Vec<CaptureTarget>> {
        Ok(vec![])
    }
}
