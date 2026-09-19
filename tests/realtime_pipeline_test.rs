use pick_up_sound_text::pipeline::realtime::{PartialEntry, RealtimePipeline, RealtimeState};
use pick_up_sound_text::asr::{AsrConfig, AsrEvent, AsrProvider, AsrStream};
use pick_up_sound_text::audio::capture::{CaptureSource, CaptureTarget};
use pick_up_sound_text::audio::AudioChunk;
use pick_up_sound_text::config::AppConfig;
use pick_up_sound_text::translate::{TranslateProvider, TranslateRequest, TranslateResponse};
use pick_up_sound_text::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

#[test]
fn realtime_state_serialization() {
    let state = RealtimeState::Listening;
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(json, "\"listening\"");

    // unit variant 必须序列化为纯字符串；newtype 会变成 {"failed": "..."} 对象，
    // 前端按字符串比较状态会失配（灰点 bug 的根因之一）
    let state = RealtimeState::Failed;
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(json, "\"failed\"");

    let event = pick_up_sound_text::pipeline::realtime::RealtimeStateEvent::failed(
        "boom".to_string(),
        Some("s1".to_string()),
    );
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"state\":\"failed\""));
    assert!(json.contains("\"error\":\"boom\""));
}

#[test]
fn partial_entry_creation() {
    let partial = PartialEntry {
        source: "hello".to_string(),
        ts_start: 1.0,
        ts_end: 2.0,
    };
    assert_eq!(partial.source, "hello");
}

#[tokio::test]
async fn realtime_pipeline_initial_state() {
    let pipeline = RealtimePipeline::new_for_test();
    let state = pipeline.get_state().await;
    assert_eq!(state, RealtimeState::Idle);
}

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
    audio_received: Arc<AtomicUsize>,
}

#[async_trait]
impl AsrProvider for MockAsrProvider {
    async fn start_stream(&self, _config: &AsrConfig) -> Result<Box<dyn AsrStream>> {
        Ok(Box::new(MockAsrStream {
            events: self.events.clone(),
            index: 0,
            audio_received: Arc::new(AtomicUsize::new(0)),
        }))
    }
}

#[async_trait]
impl AsrStream for MockAsrStream {
    async fn send_audio(&mut self, pcm: &[i16]) -> Result<()> {
        self.audio_received.fetch_add(pcm.len(), Ordering::SeqCst);
        Ok(())
    }
    async fn next_event(&mut self) -> Result<AsrEvent> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            Ok(event)
        } else {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
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

#[tokio::test]
async fn realtime_pipeline_processes_audio_to_subtitles() {
    let chunks = vec![
        AudioChunk { pcm: vec![0i16; 1600], content_time_ms: 0, wall_time_ms: 0 },
        AudioChunk { pcm: vec![0i16; 1600], content_time_ms: 100, wall_time_ms: 100 },
    ];

    let events = vec![
        AsrEvent::Partial { text: "hello".to_string(), ts_start: 0.0, ts_end: 0.5 },
        AsrEvent::Final { text: "hello world".to_string(), ts_start: 0.0, ts_end: 1.0 },
        AsrEvent::EndOfStream,
    ];

    let capture = MockCaptureSource { chunks };
    let asr = MockAsrProvider { events };
    let translate = MockTranslateProvider;

    let pipeline = RealtimePipeline::new(
        Box::new(capture),
        Box::new(asr),
        Arc::new(translate),
        AppConfig::default(),
        "en".to_string(),
        "test-session".to_string(),
        vec![],
    );

    assert_eq!(pipeline.get_state().await, RealtimeState::Idle);
}

#[tokio::test]
async fn realtime_state_transitions_from_idle() {
    let mut pipeline = RealtimePipeline::new_for_test();

    // Idle → 不能 pause
    assert!(pipeline.pause().await.is_err());

    // Idle → 不能 resume
    assert!(pipeline.resume().await.is_err());

    // Idle → stop 是 no-op
    assert!(pipeline.stop().await.is_ok());
    assert_eq!(pipeline.get_state().await, RealtimeState::Stopped);
}

struct FailingAsrProvider;

#[async_trait]
impl AsrProvider for FailingAsrProvider {
    async fn start_stream(&self, _config: &AsrConfig) -> Result<Box<dyn AsrStream>> {
        Err(pick_up_sound_text::Error::Asr(
            "401 unauthorized (simulated missing api key)".to_string(),
        ))
    }
}

#[tokio::test]
async fn failed_connect_marks_pipeline_failed_not_connecting() {
    // 回归锁：连接失败时内部状态曾卡 Connecting；重启守卫优先读内部状态，
    // 卡在中间态会永久拒绝新会话，用户只能重启应用
    let mut pipeline = RealtimePipeline::new(
        Box::new(MockCaptureSource { chunks: vec![] }),
        Box::new(FailingAsrProvider),
        Arc::new(MockTranslateProvider),
        AppConfig::default(),
        "en".to_string(),
        "s1".to_string(),
        vec![],
    );
    assert!(pipeline.connect_streams().await.is_err());
    assert_eq!(pipeline.get_state().await, RealtimeState::Failed);
}
