use pick_up_sound_text::pipeline::realtime::{PartialEntry, RealtimePipeline, RealtimeState};

#[test]
fn realtime_state_serialization() {
    let state = RealtimeState::Listening;
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(json, "\"listening\"");

    let state = RealtimeState::Failed("test error".to_string());
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("failed"));
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
