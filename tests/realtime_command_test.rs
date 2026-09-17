use pick_up_sound_text::pipeline::realtime::RealtimeState;

#[test]
fn realtime_state_info_serialization() {
    let state = RealtimeState::Listening;
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(json, "\"listening\"");
}
