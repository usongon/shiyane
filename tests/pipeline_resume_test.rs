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
use tokio_util::sync::CancellationToken;

fn fp(source_language: &str, target_lang: &str) -> CheckpointFingerprint {
    CheckpointFingerprint {
        source_language: source_language.to_string(),
        target_lang: target_lang.to_string(),
        translate_provider: "openai".to_string(),
        translate_model: "gpt-3.5-turbo".to_string(),
        asr_model: "qwen-audio-3.0-asr-flash-filetrans".to_string(),
        diarization: false,
    }
}

fn fp_d(source_language: &str, target_lang: &str, diarization: bool) -> CheckpointFingerprint {
    CheckpointFingerprint { diarization, ..fp(source_language, target_lang) }
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

/// 命中 fail_texts 的句子永远失败（模拟服务端对特定输入 >60s 无响应后超时），
/// 其余正常返回——用于钉住「单句卡死不拖死整个任务」的隔离语义
struct StallTranslate {
    fail_texts: Vec<String>,
    call_texts: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl TranslateProvider for StallTranslate {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        self.call_texts.lock().unwrap().push(req.text.clone());
        if self.fail_texts.iter().any(|t| *t == req.text) {
            return Err(Error::Translate("mock 单句卡死超时".to_string()));
        }
        Ok(TranslateResponse {
            translated_text: format!("[T] {}", req.text),
        })
    }
    async fn test_connection(&self) -> Result<()> {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn build_stall_pipeline(
    dir: &Path,
    task_id: &str,
    fingerprint: CheckpointFingerprint,
    asr_sentences: Vec<TranscriptionSentence>,
    fail_texts: Vec<String>,
    diarization: bool,
    tag: &str,
) -> (
    FilePipeline,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<StdMutex<Vec<String>>>,
) {
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
        Box::new(StallTranslate {
            fail_texts,
            call_texts: call_texts.clone(),
        }),
        AppConfig::default(),
        fingerprint.source_language.clone(),
        diarization,
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

fn build_pipeline(
    dir: &Path,
    task_id: &str,
    fingerprint: CheckpointFingerprint,
    asr_sentences: Vec<TranscriptionSentence>,
    fail_on: Option<usize>,
    diarization: bool,
    tag: &str,
) -> (
    FilePipeline,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<StdMutex<Vec<String>>>,
) {
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
        diarization,
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
        false,
        "fresh",
    );

    p.process().await.unwrap();
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Completed
    );
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
async fn stalled_sentence_falls_back_to_source_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _extract, _asr, _calls) = build_stall_pipeline(
        dir.path(),
        "t-fb1",
        fp("auto", "zh"),
        sentences(4),
        vec!["s1".to_string()],
        false,
        "fb1",
    );

    p.process().await.unwrap();
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Completed,
        "单句卡死不得拖死整个任务"
    );

    let entries = p.get_entries().await;
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].translated, "[T] s0");
    assert_eq!(entries[1].translated, "s1", "卡死句回退为原文");
    assert_eq!(entries[3].translated, "[T] s3");

    let cp = Checkpoint::load(&dir.path().join("t-fb1/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert!(cp.is_all_completed());
    assert!(!cp.segments[0].fallback);
    assert!(cp.segments[1].fallback, "卡死句要带 fallback 标记");
    assert!(!cp.segments[2].fallback);
}

#[tokio::test]
async fn five_consecutive_stalls_abort_with_honest_error() {
    let dir = tempfile::tempdir().unwrap();
    let all_fail: Vec<String> = (0..7).map(|i| format!("s{i}")).collect();
    let (mut p, _extract, _asr, calls) = build_stall_pipeline(
        dir.path(),
        "t-brk",
        fp("auto", "zh"),
        sentences(7),
        all_fail,
        false,
        "brk",
    );

    let err = p.process().await.unwrap_err();
    assert!(
        err.to_string().contains("连续 5 句"),
        "熔断文案应含「连续 5 句」，实际: {err}"
    );
    match p.get_state().await {
        pick_up_sound_text::pipeline::PipelineState::Failed(msg) => {
            assert!(msg.contains("连续 5 句"), "Failed 文案: {msg}");
        }
        other => panic!("连续卡死应 Failed，实际: {other:?}"),
    }

    let cp = Checkpoint::load(&dir.path().join("t-brk/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(cp.completed_count(), 5, "熔断前 5 句已回退落盘");
    assert_eq!(
        cp.segments[5].status,
        pick_up_sound_text::checkpoint::SegmentStatus::Pending
    );
    assert_eq!(calls.lock().unwrap().len(), 5, "熔断后不再发请求");
}

#[tokio::test]
async fn non_consecutive_stalls_complete_with_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _extract, _asr, _calls) = build_stall_pipeline(
        dir.path(),
        "t-fb2",
        fp("auto", "zh"),
        sentences(5),
        vec!["s0".to_string(), "s1".to_string(), "s3".to_string()],
        false,
        "fb2",
    );

    p.process().await.unwrap();
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Completed,
        "非连续卡死（最长连击 2）不得熔断"
    );

    let entries = p.get_entries().await;
    assert_eq!(entries[0].translated, "s0");
    assert_eq!(entries[2].translated, "[T] s2");
    assert_eq!(entries[3].translated, "s3");

    let cp = Checkpoint::load(&dir.path().join("t-fb2/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(
        cp.segments.iter().filter(|s| s.fallback).count(),
        3,
        "3 句 fallback 落盘"
    );
}

#[tokio::test]
async fn fallback_sentences_survive_resume_without_retranslate() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p1, _e1, _asr1, _c1) = build_stall_pipeline(
        dir.path(),
        "t-fb3",
        fp("auto", "zh"),
        sentences(4),
        vec!["s1".to_string()],
        false,
        "f3a",
    );
    p1.process().await.unwrap();

    let (mut p2, extract2, asr2, calls2) = build_stall_pipeline(
        dir.path(),
        "t-fb3",
        fp("auto", "zh"),
        sentences(4),
        vec![],
        false,
        "f3b",
    );
    p2.process().await.unwrap();

    assert_eq!(asr2.load(Ordering::SeqCst), 0);
    assert_eq!(extract2.load(Ordering::SeqCst), 0);
    assert!(
        calls2.lock().unwrap().is_empty(),
        "fallback 句按已完成处理，续跑不得重翻（不重复付费）"
    );
    let entries = p2.get_entries().await;
    assert_eq!(entries[1].translated, "s1", "fallback 结果原样恢复");
}

#[tokio::test]
async fn config_change_retranslate_clears_fallback_flags() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p1, _e1, _asr1, _c1) = build_stall_pipeline(
        dir.path(),
        "t-fb4",
        fp("auto", "zh"),
        sentences(4),
        vec!["s1".to_string()],
        false,
        "f4a",
    );
    p1.process().await.unwrap();

    let (mut p2, _e2, asr2, calls2) = build_stall_pipeline(
        dir.path(),
        "t-fb4",
        fp("auto", "en"),
        sentences(4),
        vec![],
        false,
        "f4b",
    );
    p2.process().await.unwrap();

    assert_eq!(asr2.load(Ordering::SeqCst), 0, "改目标语言不得重跑 ASR");
    assert_eq!(calls2.lock().unwrap().len(), 4, "全部句子重翻");

    let cp = Checkpoint::load(&dir.path().join("t-fb4/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert!(
        cp.segments.iter().all(|s| !s.fallback),
        "重翻后旧的 fallback 标记必须清零"
    );
}

#[tokio::test]
async fn resume_skips_asr_and_translates_rest() {
    let dir = tempfile::tempdir().unwrap();
    // 第一次：全部句子卡死 → 熔断中止，盘上前 5 句回退 Completed、后 2 句 Pending
    let all_fail: Vec<String> = (0..7).map(|i| format!("s{i}")).collect();
    let (mut p1, _e1, asr1, _c1) = build_stall_pipeline(
        dir.path(),
        "t-resume",
        fp("auto", "zh"),
        sentences(7),
        all_fail,
        false,
        "r1",
    );
    assert!(p1.process().await.is_err());
    assert_eq!(asr1.load(Ordering::SeqCst), 1);

    // 第二次：同目录同指纹，全新 provider 实例（计数归零）
    let (mut p2, extract2, asr2, calls2) = build_stall_pipeline(
        dir.path(),
        "t-resume",
        fp("auto", "zh"),
        sentences(7),
        vec![],
        false,
        "r2",
    );
    p2.process().await.unwrap();

    assert_eq!(asr2.load(Ordering::SeqCst), 0, "续跑必须跳过 ASR");
    assert_eq!(extract2.load(Ordering::SeqCst), 0, "续跑必须跳过音频提取");
    let texts = calls2.lock().unwrap().clone();
    assert_eq!(texts, vec!["s5", "s6"], "只翻剩余句子（回退句不重翻）");
    assert_eq!((p2.get_progress().await * 100.0).round() as i64, 100);

    let entries = p2.get_entries().await;
    assert_eq!(entries.len(), 7);
    assert_eq!(entries[5].translated, "[T] s5");
    assert_eq!(entries[6].translated, "[T] s6");
    assert_eq!(entries[0].translated, "s0", "重建的回退句保留原文");
}

#[tokio::test]
async fn all_completed_restores_instantly_without_api_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p1, _e1, _asr1, _c1) = build_pipeline(
        dir.path(),
        "t-done",
        fp("auto", "zh"),
        sentences(4),
        None,
        false,
        "d1",
    );
    p1.process().await.unwrap();

    let (mut p2, extract2, asr2, calls2) = build_pipeline(
        dir.path(),
        "t-done",
        fp("auto", "zh"),
        sentences(4),
        None,
        false,
        "d2",
    );
    p2.process().await.unwrap();

    assert_eq!(asr2.load(Ordering::SeqCst), 0);
    assert_eq!(extract2.load(Ordering::SeqCst), 0);
    assert!(calls2.lock().unwrap().is_empty());
    assert_eq!(
        p2.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Completed
    );
    let entries = p2.get_entries().await;
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[3].translated, "[T] s3");
}

#[tokio::test]
async fn target_lang_change_keeps_asr_retranslates_all() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p1, _e1, _asr1, _c1) = build_pipeline(
        dir.path(),
        "t-lang",
        fp("auto", "zh"),
        sentences(4),
        None,
        false,
        "l1",
    );
    p1.process().await.unwrap();

    let (mut p2, extract2, asr2, calls2) = build_pipeline(
        dir.path(),
        "t-lang",
        fp("auto", "en"),
        sentences(4),
        None,
        false,
        "l2",
    );
    p2.process().await.unwrap();

    assert_eq!(asr2.load(Ordering::SeqCst), 0, "改目标语言不得重跑 ASR");
    assert_eq!(extract2.load(Ordering::SeqCst), 0);
    assert_eq!(calls2.lock().unwrap().len(), 4, "全部句子重翻");

    // checkpoint 指纹已更新为新语言
    let cp = Checkpoint::load(&dir.path().join("t-lang/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(cp.fingerprint.target_lang, "en");
}

#[tokio::test]
async fn source_language_change_reruns_asr() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p1, _e1, _asr1, _c1) = build_pipeline(
        dir.path(),
        "t-src",
        fp("auto", "zh"),
        sentences(4),
        None,
        false,
        "s1",
    );
    p1.process().await.unwrap();

    let (mut p2, _extract2, asr2, calls2) = build_pipeline(
        dir.path(),
        "t-src",
        fp("ja", "zh"),
        sentences(4),
        None,
        false,
        "s2",
    );
    p2.process().await.unwrap();

    assert_eq!(asr2.load(Ordering::SeqCst), 1, "改源语言必须重跑 ASR");
    assert_eq!(calls2.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn zero_sentence_asr_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, _extract, _asr, calls) = build_pipeline(
        dir.path(),
        "t-zero",
        fp("auto", "zh"),
        sentences(0), // MockFileAsr 返回 0 句
        None,
        false,
        "zero",
    );

    let err = p.process().await.unwrap_err();
    assert!(
        err.to_string().contains("未识别到语音"),
        "错误文案应含「未识别到语音」，实际: {err}"
    );
    match p.get_state().await {
        pick_up_sound_text::pipeline::PipelineState::Failed(msg) => {
            assert!(msg.contains("未识别到语音"), "Failed 文案: {msg}");
        }
        other => panic!("零句 ASR 应进入 Failed，实际: {other:?}"),
    }
    assert!(calls.lock().unwrap().is_empty(), "零句时不得调用翻译");
}

/// 共享 token 槽：测试在构造管线后把 cancel_handle 塞进来，
/// mock 在第 n 次调用（0 起）先取消再挂起，模拟「请求在飞行中被暂停」
type SharedToken = Arc<StdMutex<Option<CancellationToken>>>;

/// 第 n 次调用（0 起）先取消再「立即返回结果」：
/// 取消与已就绪（已付费）结果同时存在，钉住「收割结果优先于丢弃」
struct HarvestCancelOnTranslate {
    n: usize,
    token: SharedToken,
    call_texts: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl TranslateProvider for HarvestCancelOnTranslate {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        let k = self.call_texts.lock().unwrap().len();
        self.call_texts.lock().unwrap().push(req.text.clone());
        if k == self.n {
            if let Some(t) = self.token.lock().unwrap().as_ref() {
                t.cancel();
            }
        }
        Ok(TranslateResponse {
            translated_text: format!("[T] {}", req.text),
        })
    }
    async fn test_connection(&self) -> Result<()> {
        Ok(())
    }
}

struct CancelOnTranslate {
    n: usize,
    token: SharedToken,
    call_texts: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl TranslateProvider for CancelOnTranslate {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        let k = self.call_texts.lock().unwrap().len();
        self.call_texts.lock().unwrap().push(req.text.clone());
        if k == self.n {
            if let Some(t) = self.token.lock().unwrap().as_ref() {
                t.cancel();
            }
            std::future::pending().await
        } else {
            Ok(TranslateResponse {
                translated_text: format!("[T] {}", req.text),
            })
        }
    }
    async fn test_connection(&self) -> Result<()> {
        Ok(())
    }
}

struct CancelOnAsr {
    token: SharedToken,
    call_count: Arc<AtomicUsize>,
}

#[async_trait]
impl FileAsrProvider for CancelOnAsr {
    async fn transcribe_file(
        &self,
        _config: &AsrConfig,
        _audio_path: &Path,
    ) -> Result<FileTranscriptionResult> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if let Some(t) = self.token.lock().unwrap().as_ref() {
            t.cancel();
        }
        std::future::pending().await
    }
}

struct CancelOnExtract {
    token: SharedToken,
    extract_count: Arc<AtomicUsize>,
}

#[async_trait]
impl AudioSource for CancelOnExtract {
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
        Some(Duration::from_secs(600))
    }
    async fn extract_full_audio_to_wav(&self) -> Result<PathBuf> {
        self.extract_count.fetch_add(1, Ordering::SeqCst);
        if let Some(t) = self.token.lock().unwrap().as_ref() {
            t.cancel();
        }
        std::future::pending().await
    }
}

/// 把管线的 cancel_handle 注入共享槽（构造后、process 前调用）
fn share_cancel(p: &FilePipeline, slot: &SharedToken) {
    *slot.lock().unwrap() = Some(p.cancel_handle());
}

#[tokio::test]
async fn pause_during_translate_parks_state_and_keeps_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let slot: SharedToken = Arc::new(StdMutex::new(None));
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let extract = Arc::new(AtomicUsize::new(0));
    let asr_count = Arc::new(AtomicUsize::new(0));
    let mut p = FilePipeline::new(
        Box::new(MockAudioSource {
            extract_count: extract.clone(),
            tag: "pz1".to_string(),
        }),
        Box::new(MockFileAsr {
            sentences: sentences(5),
            call_count: asr_count.clone(),
        }),
        Box::new(CancelOnTranslate {
            n: 2,
            token: slot.clone(),
            call_texts: calls.clone(),
        }),
        AppConfig::default(),
        "auto".to_string(),
        false,
    );
    p.init_checkpoint_in(
        dir.path().to_path_buf(),
        "t-pause-tx".to_string(),
        PathBuf::from("/tmp/resume_test_video.mp4"),
        fp("auto", "zh"),
    )
    .unwrap();
    share_cancel(&p, &slot);
    let progress = p.progress_handle();

    // 挂起保护：取消机制若失效，飞行中的请求永不返回，测试超时兜底
    let outcome = tokio::time::timeout(Duration::from_secs(5), p.process()).await;

    assert_eq!(outcome.expect("暂停必须在超时内返回").unwrap(), ());
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Paused
    );
    assert_eq!((p.get_progress().await * 100.0).round() as i64, 40);
    assert_eq!(*progress.lock().await, 0.4);

    // 盘上精确保留已完成的 2 句，飞行中的第 3 句不得落盘
    let cp = Checkpoint::load(&dir.path().join("t-pause-tx/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(cp.completed_count(), 2);
    assert_eq!(cp.segments.len(), 5);
    assert_eq!(
        cp.segments[2].status,
        pick_up_sound_text::checkpoint::SegmentStatus::Pending
    );
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn pause_during_asr_returns_paused_without_persisting_sentences() {
    let dir = tempfile::tempdir().unwrap();
    let slot: SharedToken = Arc::new(StdMutex::new(None));
    let extract = Arc::new(AtomicUsize::new(0));
    let asr_count = Arc::new(AtomicUsize::new(0));
    let mut p = FilePipeline::new(
        Box::new(MockAudioSource {
            extract_count: extract.clone(),
            tag: "pz2".to_string(),
        }),
        Box::new(CancelOnAsr {
            token: slot.clone(),
            call_count: asr_count.clone(),
        }),
        Box::new(MockTranslate {
            fail_on: None,
            call_texts: Arc::new(StdMutex::new(Vec::new())),
        }),
        AppConfig::default(),
        "auto".to_string(),
        false,
    );
    p.init_checkpoint_in(
        dir.path().to_path_buf(),
        "t-pause-asr".to_string(),
        PathBuf::from("/tmp/resume_test_video.mp4"),
        fp("auto", "zh"),
    )
    .unwrap();
    share_cancel(&p, &slot);

    let outcome = tokio::time::timeout(Duration::from_secs(5), p.process()).await;

    assert_eq!(outcome.expect("暂停必须在超时内返回").unwrap(), ());
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Paused
    );
    assert_eq!(p.get_phase().await, Phase::Transcribing);
    assert_eq!(asr_count.load(Ordering::SeqCst), 1);
    // 转写无句级断点：暂停时不得有任何句子进度（续跑=重新上传+转写）
    let persisted = Checkpoint::load(&dir.path().join("t-pause-asr/progress.jsonl")).unwrap();
    assert!(
        persisted
            .as_ref()
            .map(|c| c.segments.is_empty())
            .unwrap_or(true),
        "转写中暂停不应落盘句子，实际: {persisted:?}"
    );
}

#[tokio::test]
async fn pause_during_extract_returns_paused() {
    let dir = tempfile::tempdir().unwrap();
    let slot: SharedToken = Arc::new(StdMutex::new(None));
    let extract = Arc::new(AtomicUsize::new(0));
    let asr_count = Arc::new(AtomicUsize::new(0));
    let mut p = FilePipeline::new(
        Box::new(CancelOnExtract {
            token: slot.clone(),
            extract_count: extract.clone(),
        }),
        Box::new(MockFileAsr {
            sentences: sentences(5),
            call_count: asr_count.clone(),
        }),
        Box::new(MockTranslate {
            fail_on: None,
            call_texts: Arc::new(StdMutex::new(Vec::new())),
        }),
        AppConfig::default(),
        "auto".to_string(),
        false,
    );
    p.init_checkpoint_in(
        dir.path().to_path_buf(),
        "t-pause-ex".to_string(),
        PathBuf::from("/tmp/resume_test_video.mp4"),
        fp("auto", "zh"),
    )
    .unwrap();
    share_cancel(&p, &slot);

    let outcome = tokio::time::timeout(Duration::from_secs(5), p.process()).await;

    assert_eq!(outcome.expect("暂停必须在超时内返回").unwrap(), ());
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Paused
    );
    assert_eq!(p.get_phase().await, Phase::Extracting);
    assert_eq!(extract.load(Ordering::SeqCst), 1);
    assert_eq!(asr_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn resume_after_pause_completes_without_asr_rerun() {
    let dir = tempfile::tempdir().unwrap();
    // 第一次：第 3 句飞行中暂停 → 盘上 2 句 Completed
    let slot: SharedToken = Arc::new(StdMutex::new(None));
    let calls1 = Arc::new(StdMutex::new(Vec::new()));
    let extract1 = Arc::new(AtomicUsize::new(0));
    let asr1 = Arc::new(AtomicUsize::new(0));
    let mut p1 = FilePipeline::new(
        Box::new(MockAudioSource {
            extract_count: extract1.clone(),
            tag: "pz3a".to_string(),
        }),
        Box::new(MockFileAsr {
            sentences: sentences(5),
            call_count: asr1.clone(),
        }),
        Box::new(CancelOnTranslate {
            n: 2,
            token: slot.clone(),
            call_texts: calls1.clone(),
        }),
        AppConfig::default(),
        "auto".to_string(),
        false,
    );
    p1.init_checkpoint_in(
        dir.path().to_path_buf(),
        "t-pause-rs".to_string(),
        PathBuf::from("/tmp/resume_test_video.mp4"),
        fp("auto", "zh"),
    )
    .unwrap();
    share_cancel(&p1, &slot);
    p1.process().await.unwrap();
    assert_eq!(
        p1.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Paused
    );

    // 第二次：普通 provider 续跑，必须跳过提取/ASR，只翻剩余句子
    let (mut p2, extract2, asr2, calls2) = build_pipeline(
        dir.path(),
        "t-pause-rs",
        fp("auto", "zh"),
        sentences(5),
        None,
        false,
        "pz3b",
    );
    p2.process().await.unwrap();

    assert_eq!(
        p2.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Completed
    );
    assert_eq!(asr2.load(Ordering::SeqCst), 0, "暂停后续跑必须跳过 ASR");
    assert_eq!(extract2.load(Ordering::SeqCst), 0);
    assert_eq!(
        calls2.lock().unwrap().clone(),
        vec!["s2", "s3", "s4"],
        "只翻剩余句子"
    );
    let entries = p2.get_entries().await;
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[2].translated, "[T] s2");
}

#[tokio::test]
async fn pre_cancelled_token_pauses_before_any_work() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, extract, asr_count, calls) = build_pipeline(
        dir.path(),
        "t-pre-cancel",
        fp("auto", "zh"),
        sentences(5),
        None,
        false,
        "pc1",
    );
    p.cancel_handle().cancel(); // process 启动前已取消

    let outcome = tokio::time::timeout(Duration::from_secs(5), p.process()).await;

    assert_eq!(outcome.expect("必须立即返回").unwrap(), ());
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Paused
    );
    assert_eq!(extract.load(Ordering::SeqCst), 0, "取消后不得启动音频提取");
    assert_eq!(asr_count.load(Ordering::SeqCst), 0, "取消后不得启动转写");
    assert!(calls.lock().unwrap().is_empty(), "取消后不得发起翻译");
}

#[tokio::test]
async fn cancel_with_ready_response_harvests_paid_work() {
    let dir = tempfile::tempdir().unwrap();
    let slot: SharedToken = Arc::new(StdMutex::new(None));
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let extract = Arc::new(AtomicUsize::new(0));
    let asr_count = Arc::new(AtomicUsize::new(0));
    let mut p = FilePipeline::new(
        Box::new(MockAudioSource {
            extract_count: extract.clone(),
            tag: "hv1".to_string(),
        }),
        Box::new(MockFileAsr {
            sentences: sentences(5),
            call_count: asr_count.clone(),
        }),
        Box::new(HarvestCancelOnTranslate {
            n: 2,
            token: slot.clone(),
            call_texts: calls.clone(),
        }),
        AppConfig::default(),
        "auto".to_string(),
        false,
    );
    p.init_checkpoint_in(
        dir.path().to_path_buf(),
        "t-harvest".to_string(),
        PathBuf::from("/tmp/resume_test_video.mp4"),
        fp("auto", "zh"),
    )
    .unwrap();
    share_cancel(&p, &slot);

    p.process().await.unwrap();

    // s0/s1 正常完成；s2 结果已就绪 → 收割落盘；s3 不再发起新请求
    assert_eq!(
        p.get_state().await,
        pick_up_sound_text::pipeline::PipelineState::Paused
    );
    assert_eq!(calls.lock().unwrap().len(), 3);
    let cp = Checkpoint::load(&dir.path().join("t-harvest/progress.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(cp.completed_count(), 3, "已就绪的付费结果必须收割");
    assert_eq!(
        cp.segments[3].status,
        pick_up_sound_text::checkpoint::SegmentStatus::Pending
    );
}

#[tokio::test]
async fn resume_seeds_translation_context_from_checkpoint() {
    /// 记录每次翻译请求的 context（包一层 MockTranslate）
    struct CtxCapture {
        inner: MockTranslate,
        contexts: Arc<StdMutex<Vec<Vec<String>>>>,
    }
    #[async_trait]
    impl TranslateProvider for CtxCapture {
        async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
            self.contexts.lock().unwrap().push(req.context.clone());
            self.inner.translate(req).await
        }
        async fn test_connection(&self) -> Result<()> {
            Ok(())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    // 第一次：全部句子卡死 → 熔断中止，盘上前 5 句回退 Completed（含 s0、s1）
    let all_fail: Vec<String> = (0..7).map(|i| format!("s{i}")).collect();
    let (mut p1, _e1, _asr1, _c1) = build_stall_pipeline(
        dir.path(),
        "t-ctx",
        fp("auto", "zh"),
        sentences(7),
        all_fail,
        false,
        "x1",
    );
    assert!(p1.process().await.is_err());

    let contexts: Arc<StdMutex<Vec<Vec<String>>>> = Arc::new(StdMutex::new(Vec::new()));
    let extract = Arc::new(AtomicUsize::new(0));
    let asr_count = Arc::new(AtomicUsize::new(0));
    let call_texts = Arc::new(StdMutex::new(Vec::new()));
    let mut p2 = FilePipeline::new(
        Box::new(MockAudioSource {
            extract_count: extract.clone(),
            tag: "x2".to_string(),
        }),
        Box::new(MockFileAsr {
            sentences: sentences(5),
            call_count: asr_count.clone(),
        }),
        Box::new(CtxCapture {
            inner: MockTranslate {
                fail_on: None,
                call_texts: call_texts.clone(),
            },
            contexts: contexts.clone(),
        }),
        AppConfig::default(),
        "auto".to_string(),
        false,
    );
    p2.init_checkpoint_in(
        dir.path().to_path_buf(),
        "t-ctx".to_string(),
        PathBuf::from("/tmp/resume_test_video.mp4"),
        fp("auto", "zh"),
    )
    .unwrap();
    p2.process().await.unwrap();

    // 续翻首句（s2）的 context 必须含已完成句 s0、s1（跨重启保持翻译上下文）
    let all = contexts.lock().unwrap();
    assert!(!all.is_empty());
    let first = &all[0];
    assert!(
        first.contains(&"s0".to_string()) && first.contains(&"s1".to_string()),
        "续翻首句 context 应含 s0/s1，实际: {first:?}"
    );
}

#[tokio::test]
async fn diarization_flip_invalidates_asr_but_same_flag_resumes() {
    let dir = tempfile::tempdir().unwrap();

    // 第一遍：开关关，跑完
    let (mut p1, _e1, asr1, _c1) = build_pipeline(dir.path(), "t-diar", fp("auto", "zh"), sentences(5), None, false, "diar1");
    p1.process().await.unwrap();
    assert_eq!(asr1.load(Ordering::SeqCst), 1);

    // 同开关重跑（已完成）：瞬时恢复，ASR 0 次
    let (mut p2, _e2, asr2, _c2) = build_pipeline(dir.path(), "t-diar", fp("auto", "zh"), sentences(5), None, false, "diar2");
    p2.process().await.unwrap();
    assert_eq!(asr2.load(Ordering::SeqCst), 0);

    // 开关翻转：asr_invalid → 全量重跑，ASR 1 次
    let (mut p3, _e3, asr3, _c3) = build_pipeline(dir.path(), "t-diar", fp_d("auto", "zh", true), sentences(5), None, true, "diar3");
    p3.process().await.unwrap();
    assert_eq!(asr3.load(Ordering::SeqCst), 1);
}
