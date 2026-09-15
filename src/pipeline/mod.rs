//! File-mode pipeline state machine.
//!
//! Orchestrates audio source -> ASR -> translation -> subtitle entries.
//! Also owns overlap de-duplication between consecutive ASR results
//! (Ruling: implemented here, not in `FileAudioSource`).

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::asr::{AsrConfig, FileAsrProvider};
use crate::audio::AudioSource;
use crate::checkpoint::{Checkpoint, SegmentStatus};
use crate::config::AppConfig;
use crate::error::Error;
use crate::subtitle::{SubtitleEntry, SubtitleStatus};
use crate::translate::{TranslateProvider, TranslateRequest};
use crate::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum PipelineState {
    Idle,
    Processing,
    Completed,
    Exported,
    Failed(String),
}

pub struct FilePipeline {
    state: Arc<Mutex<PipelineState>>,
    audio_source: Box<dyn AudioSource>,
    asr_provider: Box<dyn FileAsrProvider>,
    translate_provider: Box<dyn TranslateProvider>,
    entries: Arc<Mutex<Vec<SubtitleEntry>>>,
    config: AppConfig,
    source_language: String,
    checkpoint: Option<Checkpoint>,
    checkpoint_path: Option<PathBuf>,
    total_segments: usize,
    completed_segments: usize,
    progress: Arc<Mutex<f64>>,
}

impl FilePipeline {
    pub fn new(
        audio_source: Box<dyn AudioSource>,
        asr_provider: Box<dyn FileAsrProvider>,
        translate_provider: Box<dyn TranslateProvider>,
        config: AppConfig,
        source_language: String,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(PipelineState::Idle)),
            audio_source,
            asr_provider,
            translate_provider,
            entries: Arc::new(Mutex::new(Vec::new())),
            config,
            source_language,
            checkpoint: None,
            checkpoint_path: None,
            total_segments: 0,
            completed_segments: 0,
            progress: Arc::new(Mutex::new(0.0)),
        }
    }
    
    /// Initialize checkpoint for resume from breakpoint
    pub fn init_checkpoint(&mut self, task_id: String, video_path: PathBuf) -> Result<()> {
        let checkpoint_dir = dirs::home_dir()
            .ok_or_else(|| Error::Config("Cannot find home directory".to_string()))?
            .join("Library/Application Support/pick-up-sound-text/tasks")
            .join(&task_id);
        
        std::fs::create_dir_all(&checkpoint_dir)?;
        let checkpoint_path = checkpoint_dir.join("progress.jsonl");
        
        let checkpoint = match Checkpoint::load(&checkpoint_path)? {
            Some(c) => c,
            None => Checkpoint::new(task_id, video_path, Default::default()),
        };
        
        self.checkpoint = Some(checkpoint);
        self.checkpoint_path = Some(checkpoint_path);
        
        Ok(())
    }
    
    /// Get real progress based on completed segments
    pub async fn get_progress(&self) -> f64 {
        *self.progress.lock().await
    }

    /// Shared progress handle for external polling without locking the pipeline.
    pub fn progress_handle(&self) -> Arc<Mutex<f64>> {
        self.progress.clone()
    }

    pub async fn process(&mut self) -> Result<()> {
        let mut state = self.state.lock().await;
        *state = PipelineState::Processing;
        drop(state);

        // Duration limit: min(upstream transcription limit, 3h)
        const MAX_DURATION_SECS: f64 = 3.0 * 3600.0;
        if let Some(total) = self.audio_source.total_duration() {
            if total.as_secs_f64() > MAX_DURATION_SECS {
                let msg = format!(
                    "视频时长 {:.1} 小时超过限制（最长 3 小时），请先裁剪后再处理",
                    total.as_secs_f64() / 3600.0
                );
                let mut state = self.state.lock().await;
                *state = PipelineState::Failed(msg.clone());
                return Err(Error::AudioSource(msg));
            }
        }

        // Check if already completed via checkpoint
        if let Some(checkpoint) = &self.checkpoint {
            let completed = checkpoint
                .segments
                .iter()
                .filter(|s| s.status == SegmentStatus::Completed)
                .count();

            if completed > 0 {
                tracing::info!("Found {} completed segments in checkpoint, but file transcription is atomic - starting fresh", completed);
            }
        }

        // Extract full audio to temporary WAV file
        tracing::info!("Extracting audio from video...");
        let audio_path = match self.audio_source.extract_full_audio_to_wav().await {
            Ok(path) => path,
            Err(e) => {
                let mut state = self.state.lock().await;
                *state = PipelineState::Failed(e.to_string());
                return Err(e);
            }
        };

        tracing::info!("Audio extracted to: {:?}", audio_path);

        // Audio size limit: min(OSS simple-upload max 5GB, 50GB)
        const MAX_AUDIO_BYTES: u64 = 5 * 1024 * 1024 * 1024;
        let audio_size = std::fs::metadata(&audio_path).map(|m| m.len()).unwrap_or(0);
        if audio_size > MAX_AUDIO_BYTES {
            let _ = std::fs::remove_file(&audio_path);
            let msg = format!(
                "提取的音频文件大小 {:.1}GB 超过限制（最大 5GB）",
                audio_size as f64 / 1024.0 / 1024.0 / 1024.0
            );
            let mut state = self.state.lock().await;
            *state = PipelineState::Failed(msg.clone());
            return Err(Error::AudioSource(msg));
        }

        // Prepare ASR config
        let asr_config = AsrConfig {
            provider: self.config.asr.provider.clone(),
            model: self.config.asr.file_model.clone(),
            api_key: self.config.asr.api_key.clone(),
            language: self.source_language.clone(),
            workspace_id: self.config.asr.workspace_id.clone(),
        };

        // Transcribe the complete audio file
        tracing::info!("Starting file transcription with model: {}", asr_config.model);
        let transcription = match self.asr_provider.transcribe_file(&asr_config, &audio_path).await {
            Ok(result) => result,
            Err(e) => {
                let mut state = self.state.lock().await;
                *state = PipelineState::Failed(e.to_string());
                // Clean up temp file
                let _ = std::fs::remove_file(&audio_path);
                return Err(e);
            }
        };
        
        tracing::info!("Transcription completed: {} sentences", transcription.sentences.len());

        // Clean up temp audio file
        if let Err(e) = std::fs::remove_file(&audio_path) {
            tracing::warn!("Failed to delete temp audio file {:?}: {}", audio_path, e);
        }

        // Translate each sentence and create subtitle entries
        let total_sentences = transcription.sentences.len();
        let mut entries = Vec::new();
        let mut previous_texts: Vec<String> = Vec::new();

        for (idx, sentence) in transcription.sentences.iter().enumerate() {
            tracing::info!("Translating sentence {}/{}: '{}'", idx + 1, total_sentences, sentence.text);
            
            // Build translation request with context
            let translate_req = TranslateRequest {
                text: sentence.text.clone(),
                source_lang: "auto".to_string(),
                target_lang: self.config.translate.target_lang.clone(),
                context: previous_texts.iter()
                    .rev()
                    .take(10)
                    .cloned()
                    .collect(),
                glossary: None,
            };
            
            let translate_resp = match self.translate_provider.translate(translate_req).await {
                Ok(resp) => resp,
                Err(e) => {
                    let mut state = self.state.lock().await;
                    *state = PipelineState::Failed(e.to_string());
                    return Err(e);
                }
            };
            
            // Create subtitle entry
            let entry = SubtitleEntry {
                content_start: sentence.begin_time,
                content_end: sentence.end_time,
                wall_start: (sentence.begin_time * 1000.0) as i64,
                wall_end: (sentence.end_time * 1000.0) as i64,
                source: sentence.text.clone(),
                translated: translate_resp.translated_text,
                status: SubtitleStatus::Final,
            };
            
            entries.push(entry);
            previous_texts.push(sentence.text.clone());
            
            // Update progress
            let mut p = self.progress.lock().await;
            *p = (idx + 1) as f64 / total_sentences as f64;
            drop(p);
            
            tracing::info!("Progress: {}/{} sentences = {:.1}%", idx + 1, total_sentences, ((idx + 1) as f64 / total_sentences as f64) * 100.0);
        }

        // Store entries
        let mut entries_guard = self.entries.lock().await;
        *entries_guard = entries;
        drop(entries_guard);

        // Mark as completed
        let mut state = self.state.lock().await;
        *state = PipelineState::Completed;
        
        tracing::info!("Pipeline completed successfully");

        Ok(())
    }

    pub async fn get_state(&self) -> PipelineState {
        self.state.lock().await.clone()
    }

    pub async fn get_entries(&self) -> Vec<SubtitleEntry> {
        self.entries.lock().await.clone()
    }

    /// Get a clone of the shared state Arc for external monitoring.
    pub fn state_handle(&self) -> Arc<Mutex<PipelineState>> {
        self.state.clone()
    }

    /// Get a clone of the shared entries Arc for external access.
    pub fn entries_handle(&self) -> Arc<Mutex<Vec<SubtitleEntry>>> {
        self.entries.clone()
    }
}

/// De-duplicate overlap between two consecutive ASR text segments.
///
/// Finds the longest suffix of `prev` that is also a prefix of `next`
/// (bounded by `max_overlap` chars) and returns `next` with that overlap
/// stripped. Comparison is done on trimmed lowercase text to be robust
/// against minor ASR whitespace/case jitter.
pub fn dedup_overlap(prev: &str, next: &str, max_overlap: usize) -> String {
    if prev.is_empty() || next.is_empty() || max_overlap == 0 {
        return next.to_string();
    }

    let prev_chars: Vec<char> = prev.trim_end().chars().collect();
    let next_chars: Vec<char> = next.chars().collect();

    let upper = max_overlap.min(prev_chars.len()).min(next_chars.len());
    let mut best = 0;
    for k in 1..=upper {
        let prev_suffix = &prev_chars[prev_chars.len() - k..];
        let next_prefix = &next_chars[..k];
        if prev_suffix
            .iter()
            .zip(next_prefix.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            best = k;
        }
    }

    next_chars[best..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_no_overlap() {
        assert_eq!(dedup_overlap("hello", "world", 10), "world");
    }

    #[test]
    fn dedup_with_overlap() {
        assert_eq!(dedup_overlap("hello world", "world again", 10), " again");
    }

    #[test]
    fn dedup_respects_max_overlap() {
        // overlap is 5 ("world"), but max_overlap=3 -> no dedup
        assert_eq!(
            dedup_overlap("hello world", "world again", 3),
            "world again"
        );
    }

    #[test]
    fn dedup_empty_inputs() {
        assert_eq!(dedup_overlap("", "abc", 10), "abc");
        assert_eq!(dedup_overlap("abc", "", 10), "");
    }

    #[test]
    fn dedup_case_insensitive() {
        assert_eq!(dedup_overlap("Hello World", "world again", 10), " again");
    }
}
