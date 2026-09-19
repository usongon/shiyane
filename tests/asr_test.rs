use pick_up_sound_text::asr::AsrProvider;
use pick_up_sound_text::asr::{AsrConfig, AsrEvent};
use pick_up_sound_text::asr::dashscope::DashScopeAsrProvider;

// These tests require a real DashScope API key and network access
// They are marked as ignored by default

#[tokio::test]
#[ignore]
async fn test_dashscope_provider_creation() {
    let provider = DashScopeAsrProvider;
    let config = AsrConfig {
        provider: "dashscope".to_string(),
        model: "paraformer-realtime-v2".to_string(),
        api_key: std::env::var("DASHSCOPE_API_KEY").unwrap_or_else(|_| "test_key".to_string()),
        language: "auto".to_string(),
        workspace_id: None,
        diarization: false,
    };

    let stream = provider.start_stream(&config).await;
    assert!(stream.is_ok());
}

#[tokio::test]
#[ignore]
async fn test_dashscope_stream_placeholder_event() {
    let provider = DashScopeAsrProvider;
    let config = AsrConfig {
        provider: "dashscope".to_string(),
        model: "paraformer-realtime-v2".to_string(),
        api_key: std::env::var("DASHSCOPE_API_KEY").unwrap_or_else(|_| "test_key".to_string()),
        language: "auto".to_string(),
        workspace_id: None,
        diarization: false,
    };

    let mut stream = provider.start_stream(&config).await.expect("start_stream");
    stream.send_audio(&[0i16; 160]).await.expect("send_audio");
    let event = stream.next_event().await.expect("next_event");
    match event {
        AsrEvent::Final { text, .. } => assert_eq!(text, "placeholder"),
        other => panic!("expected Final placeholder, got {:?}", other),
    }
}
