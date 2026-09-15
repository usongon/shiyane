use async_trait::async_trait;
use pick_up_sound_text::asr::{
    AsrConfig, FileAsrProvider, FileTranscriptionResult, TranscriptionSentence,
};
use pick_up_sound_text::audio::AudioSource;
use pick_up_sound_text::checkpoint::{Checkpoint, CheckpointFingerprint};
use pick_up_sound_text::config::AppConfig;
use pick_up_sound_text::pipeline::{FilePipeline, Phase};
use pick_up_sound_text::translate::{TranslateProvider, TranslateRequest, TranslateResponse};
use pick_up_sound_text::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

fn fp(source_language: &str, target_lang: &str) -> CheckpointFingerprint {
    CheckpointFingerprint {
        source_language: source_language.to_string(),
        target_lang: target_lang.to_string(),
        translate_provider: "openai".to_string(),
        translate_model: "gpt-3.5-turbo".to_string(),
        asr_model: "qwen-audio-3.0-asr-flash-filetrans".to_string(),
    }
}

fn sentences(n: usize) -> Vec<TranscriptionSentence> {
    (0..n)
        .map(|i| TranscriptionSentence {
            text: format!("s{i}"),
            begin_time: i as f64,
            end_time: i as f64 + 1.0,
        })
        .collect()
}

struct MockAudioSource {
    extract_count: Arc<AtomicUsize>,
    tag: String,
}

#[async_trait]
impl AudioSource for MockAudioSource {
    async fn next_chunk(&mut self) -> Result<pick_up_sound_text::audio::AudioChunk> {
        Err(Error::AudioSource("End of file".to_string()))
    }
    async fn seek(&mut self, _pos: Duration) -> Result<()> {
        Ok(())
    }
    fn supports_seek(&self) -> bool {
        true
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(600)) // < 3h 上限
    }
    async fn extract_full_audio_to_wav(&self) -> Result<PathBuf> {
        self.extract_count.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("resume_test_{}.wav", self.tag));
        std::fs::write(&p, b"RIFF").map_err(|e| Error::AudioSource(e.to_string()))?;
        Ok(p)
    }
}

struct MockFileAsr {
    sentences: Vec<TranscriptionSentence>,
    call_count: Arc<AtomicUsize>,
}

#[async_trait]
impl FileAsrProvider for MockFileAsr {
    async fn transcribe_file(
        &self,
        _config: &AsrConfig,
        _audio_path: &Path,
    ) -> Result<FileTranscriptionResult> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(FileTranscriptionResult {
            sentences: self.sentences.clone(),
        })
    }
}

/// fail_on: 第 N 次调用（0 起）返回错误；call_texts 记录所有请求原文
struct MockTranslate {
    fail_on: Option<usize>,
    call_texts: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl TranslateProvider for MockTranslate {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        let n = self.call_texts.lock().unwrap().len();
        self.call_texts.lock().unwrap().push(req.text.clone());
        if self.fail_on == Some(n) {
            return Err(Error::Translate("mock 翻译失败".to_string()));
        }
        Ok(TranslateResponse {
            translated_text: format!("[T] {}", req.text),
        })
    }
    async fn test_connection(&self) -> Result<()> {
        Ok(())
    }
}

fn build_pipeline(
    dir: &Path,
    task_id: &str,
    fingerprint: CheckpointFingerprint,
    asr_sentences: Vec<TranscriptionSentence>,
    fail_on: Option<usize>,
    tag: &str,
) -> (FilePipeline, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<StdMutex<Vec<String>>>) {
    let extract_count = Arc::new(AtomicUsize::new(0));
    let asr_count = Arc::new(AtomicUsize::new(0));
    let call_texts = Arc::new(StdMutex::new(Vec::new()));
    let mut pipeline = FilePipeline::new(
        Box::new(MockAudioSource {
            extract_count: extract_count.clone(),
            tag: tag.to_string(),
        }),
        Box::new(MockFileAsr {
            sentences: asr_sentences,
            call_count: asr_count.clone(),
        }),
        Box::new(MockTranslate {
            fail_on,
            call_texts: call_texts.clone(),
        }),
        AppConfig::default(),
        fingerprint.source_language.clone(),
    );
    pipeline
        .init_checkpoint_in(
            dir.to_path_buf(),
            task_id.to_string(),
            PathBuf::from("/tmp/resume_test_video.mp4"),
            fingerprint,
        )
        .unwrap();
    (pipeline, extract_count, asr_count, call_texts)
}

#[tokio::test]
async fn fresh_run_persists_completed_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _extract, asr_count, _calls) = build_pipeline(
        dir.path(),
        "t-fresh",
        fp("auto", "zh"),
        sentences(5),
        None,
        "fresh",
    );

    p.process().await.unwrap();
    assert_eq!(p.get_state().await, pick_up_sound_text::pipeline::PipelineState::Completed);
    assert_eq!(p.get_phase().await, Phase::Done);
    assert_eq!(asr_count.load(Ordering::SeqCst), 1);

    let cp_path = dir.path().join("t-fresh/progress.jsonl");
    let cp = Checkpoint::load(&cp_path).unwrap().unwrap();
    assert_eq!(cp.segments.len(), 5);
    assert!(cp.is_all_completed());
    assert_eq!(cp.segments[3].source.as_deref(), Some("s3"));
    assert_eq!(cp.segments[3].translated.as_deref(), Some("[T] s3"));
    // 完成后已压缩：meta + 5 句 = 6 行
    let content = std::fs::read_to_string(&cp_path).unwrap();
    assert_eq!(content.lines().filter(|l| !l.trim().is_empty()).count(), 6);
}

#[tokio::test]
async fn mid_run_failure_keeps_completed_sentences_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _extract, _asr, _calls) = build_pipeline(
        dir.path(),
        "t-mid",
        fp("auto", "zh"),
        sentences(5),
        Some(2), // 第 3 次翻译调用失败
        "mid",
    );

    assert!(p.process().await.is_err());

    let cp = Checkpoint::load(&dir.path().join("t-mid/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(cp.completed_count(), 2); // 前两句已落盘
    assert_eq!(cp.segments[2].status, pick_up_sound_text::checkpoint::SegmentStatus::Pending);
}
