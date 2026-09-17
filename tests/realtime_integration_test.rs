use pick_up_sound_text::asr::{AsrConfig, AsrEvent, AsrProvider, AsrStream};
use pick_up_sound_text::audio::capture::{CaptureSource, CaptureTarget};
use pick_up_sound_text::audio::AudioChunk;
use pick_up_sound_text::checkpoint::{Checkpoint, CheckpointFingerprint};
use pick_up_sound_text::config::AppConfig;
use pick_up_sound_text::pipeline::realtime::{RealtimePipeline, RealtimeState};
use pick_up_sound_text::translate::{TranslateProvider, TranslateRequest, TranslateResponse};
use pick_up_sound_text::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

// --- Mock providers ---

struct MockCaptureSource {
    chunks: Vec<AudioChunk>,
}

#[async_trait]
impl CaptureSource for MockCaptureSource {
    async fn start(&mut self, _target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>> {
        let (tx, rx) = mpsc::channel(100);
        let chunks = self.chunks.clone();
        tokio::spawn(async move {
            for chunk in chunks {
                if tx.send(chunk).await.is_err() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        Ok(rx)
    }
    async fn stop(&mut self) -> Result<()> { Ok(()) }
    async fn list_targets(&self) -> Result<Vec<CaptureTarget>> { Ok(vec![]) }
}

struct MockAsrProvider {
    events: Vec<AsrEvent>,
}

struct MockAsrStream {
    events: Vec<AsrEvent>,
    index: usize,
}

#[async_trait]
impl AsrProvider for MockAsrProvider {
    async fn start_stream(&self, _config: &AsrConfig) -> Result<Box<dyn AsrStream>> {
        Ok(Box::new(MockAsrStream { events: self.events.clone(), index: 0 }))
    }
}

#[async_trait]
impl AsrStream for MockAsrStream {
    async fn send_audio(&mut self, _pcm: &[i16]) -> Result<()> { Ok(()) }
    async fn next_event(&mut self) -> Result<AsrEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            Ok(event)
        } else {
            Ok(AsrEvent::EndOfStream)
        }
    }
    async fn finish(&mut self) -> Result<()> { Ok(()) }
}

struct MockTranslateProvider;

#[async_trait]
impl TranslateProvider for MockTranslateProvider {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        Ok(TranslateResponse {
            translated_text: format!("[译] {}", req.text),
        })
    }
    async fn test_connection(&self) -> Result<()> { Ok(()) }
}

// --- Tests ---

#[tokio::test]
async fn realtime_pipeline_initial_state_is_idle() {
    let pipeline = RealtimePipeline::new_for_test();
    assert_eq!(pipeline.get_state().await, RealtimeState::Idle);
}

#[tokio::test]
async fn realtime_checkpoint_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = "realtime-test-001".to_string();

    let mut pipeline = RealtimePipeline::new(
        Box::new(MockCaptureSource { chunks: vec![] }),
        Box::new(MockAsrProvider { events: vec![] }),
        Arc::new(MockTranslateProvider),
        AppConfig::default(),
        "en".to_string(),
        session_id.clone(),
        vec![],
    );

    let fingerprint = CheckpointFingerprint {
        source_language: "en".to_string(),
        target_lang: "zh".to_string(),
        translate_provider: "openai".to_string(),
        translate_model: "gpt-3.5-turbo".to_string(),
        asr_model: "qwen-audio-3.0-asr-flash".to_string(),
    };

    pipeline.init_checkpoint_in(dir.path().to_path_buf(), fingerprint.clone()).unwrap();

    // 验证 checkpoint 目录已创建
    let cp_dir = dir.path().join(&session_id);
    assert!(cp_dir.exists());

    // init 即落盘 meta 行：append_updates 只追加 segment 行，
    // meta 缺位会让 load 把首行 segment 误判为 meta 损坏（曾致整档作废）
    let cp_path = cp_dir.join("progress.jsonl");
    assert!(cp_path.exists());

    // 追加 segment 行后仍可正常加载（meta 在首行、segment 折叠正确）
    use pick_up_sound_text::checkpoint::{SegmentProgress, SegmentStatus};
    let _ = Checkpoint::append_updates(&cp_path, &[SegmentProgress {
        segment_id: 0,
        start_time: Some(0.0),
        end_time: Some(1.0),
        status: SegmentStatus::Pending,
        source: Some("hello".to_string()),
        translated: None,
        fallback: false,
    }]).unwrap();

    // 加载 checkpoint 验证
    let loaded = Checkpoint::load(&cp_path).unwrap();
    assert!(loaded.is_some());
    let cp = loaded.unwrap();
    assert_eq!(cp.task_id, session_id);
    assert_eq!(cp.segments.len(), 1);
    assert_eq!(cp.segments[0].source.as_deref(), Some("hello"));
}

#[tokio::test]
async fn realtime_state_transitions() {
    let mut pipeline = RealtimePipeline::new_for_test();

    // Idle → 不能 pause
    assert!(pipeline.pause().await.is_err());

    // Idle → 不能 resume
    assert!(pipeline.resume().await.is_err());

    // Idle → stop 是 no-op（状态置 Stopped）
    assert!(pipeline.stop().await.is_ok());
    assert_eq!(pipeline.get_state().await, RealtimeState::Stopped);
}

#[tokio::test]
async fn realtime_checkpoint_with_segments() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = "realtime-test-002".to_string();

    let mut pipeline = RealtimePipeline::new(
        Box::new(MockCaptureSource { chunks: vec![] }),
        Box::new(MockAsrProvider { events: vec![] }),
        Arc::new(MockTranslateProvider),
        AppConfig::default(),
        "en".to_string(),
        session_id.clone(),
        vec![],
    );

    let fingerprint = CheckpointFingerprint {
        source_language: "en".to_string(),
        target_lang: "zh".to_string(),
        translate_provider: "openai".to_string(),
        translate_model: "gpt-3.5-turbo".to_string(),
        asr_model: "qwen-audio-3.0-asr-flash".to_string(),
    };

    pipeline.init_checkpoint_in(dir.path().to_path_buf(), fingerprint.clone()).unwrap();

    // init_checkpoint_in 只创建目录并加载/新建 checkpoint 对象，不立即写文件。
    // 先通过 Checkpoint::new + save 写入 meta 行，模拟 pipeline 内部行为。
    let cp_path = dir.path().join(&session_id).join("progress.jsonl");
    let video_path = std::path::PathBuf::from(format!("realtime://{}", session_id));
    let cp = Checkpoint::new(session_id.clone(), video_path, fingerprint);
    cp.save(&cp_path).unwrap();

    // 手动添加 segment 到 checkpoint
    let seg = pick_up_sound_text::checkpoint::SegmentProgress {
        segment_id: 0,
        start_time: Some(0.0),
        end_time: Some(1.5),
        status: pick_up_sound_text::checkpoint::SegmentStatus::Pending,
        source: Some("hello world".to_string()),
        translated: None,
        fallback: false,
    };
    Checkpoint::append_updates(&cp_path, &[seg]).unwrap();

    // 重新加载验证
    let loaded = Checkpoint::load(&cp_path).unwrap().unwrap();
    assert_eq!(loaded.segments.len(), 1);
    assert_eq!(loaded.segments[0].source.as_deref(), Some("hello world"));
    assert_eq!(loaded.segments[0].status, pick_up_sound_text::checkpoint::SegmentStatus::Pending);
}
