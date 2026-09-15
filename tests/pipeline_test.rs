use pick_up_sound_text::asr::{
    AsrConfig, FileAsrProvider, FileTranscriptionResult, TranscriptionSentence,
};
use pick_up_sound_text::audio::{AudioChunk, AudioSource};
use pick_up_sound_text::config::AppConfig;
use pick_up_sound_text::pipeline::{FilePipeline, PipelineState};
use pick_up_sound_text::subtitle::SubtitleStatus;
use pick_up_sound_text::translate::{TranslateProvider, TranslateRequest, TranslateResponse};
use pick_up_sound_text::{Error, Result};
use async_trait::async_trait;
use std::time::Duration;

// Mock AudioSource that returns a single chunk then EOF
struct MockAudioSource {
    chunks: Vec<AudioChunk>,
    index: usize,
}

impl MockAudioSource {
    fn new(chunks: Vec<AudioChunk>) -> Self {
        Self { chunks, index: 0 }
    }
}

#[async_trait]
impl AudioSource for MockAudioSource {
    async fn next_chunk(&mut self) -> Result<AudioChunk> {
        if self.index >= self.chunks.len() {
            return Err(Error::AudioSource("End of file".to_string()));
        }
        let chunk = self.chunks[self.index].clone();
        self.index += 1;
        Ok(chunk)
    }

    async fn seek(&mut self, _pos: Duration) -> Result<()> {
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        true
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    async fn extract_full_audio_to_wav(&self) -> Result<std::path::PathBuf> {
        // 管线在 ASR 完成后会删除该文件，必须真实存在
        let p = std::env::temp_dir().join("pipeline_test_mock_audio.wav");
        std::fs::write(&p, b"RIFF").map_err(|e| Error::AudioSource(e.to_string()))?;
        Ok(p)
    }
}

// Mock file transcription provider (submit → poll → download 的替身)
struct MockFileAsrProvider;

#[async_trait]
impl FileAsrProvider for MockFileAsrProvider {
    async fn transcribe_file(
        &self,
        _config: &AsrConfig,
        _audio_path: &std::path::Path,
    ) -> Result<FileTranscriptionResult> {
        Ok(FileTranscriptionResult {
            sentences: vec![TranscriptionSentence {
                text: "hello world".to_string(),
                begin_time: 0.0,
                end_time: 1.0,
            }],
        })
    }
}

// Mock Translate Provider
struct MockTranslateProvider;

#[async_trait]
impl TranslateProvider for MockTranslateProvider {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        Ok(TranslateResponse {
            translated_text: format!("[Translated] {}", req.text),
        })
    }

    async fn test_connection(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn test_pipeline_state_transitions() {
    assert_eq!(PipelineState::Idle, PipelineState::Idle);
    assert_ne!(PipelineState::Idle, PipelineState::Processing);
}

#[tokio::test]
async fn test_pipeline_process_with_mock() {
    let chunks = vec![AudioChunk {
        pcm: vec![0i16; 1600],
        content_time_ms: 0,
        wall_time_ms: 1000,
    }];

    let mut pipeline = FilePipeline::new(
        Box::new(MockAudioSource::new(chunks)),
        Box::new(MockFileAsrProvider),
        Box::new(MockTranslateProvider),
        AppConfig::default(),
        "auto".to_string(),
    );

    assert_eq!(pipeline.get_state().await, PipelineState::Idle);

    let result = pipeline.process().await;
    assert!(result.is_ok());
    assert_eq!(pipeline.get_state().await, PipelineState::Completed);

    let entries = pipeline.get_entries().await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, "hello world");
    assert_eq!(entries[0].translated, "[Translated] hello world");
    assert_eq!(entries[0].status, SubtitleStatus::Final);
    assert_eq!(entries[0].content_start, 0.0);
    assert_eq!(entries[0].content_end, 1.0);
}
