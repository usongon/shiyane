use pick_up_sound_text::audio::capture::{create_capture_source, CaptureKind, CaptureTarget};

#[test]
fn capture_target_serialization() {
    let target = CaptureTarget {
        id: "chrome".to_string(),
        name: "Google Chrome".to_string(),
        kind: CaptureKind::SystemAudio,
        icon_path: None,
    };
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("system_audio"));
    let deserialized: CaptureTarget = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.kind, CaptureKind::SystemAudio);
}

#[tokio::test]
async fn create_capture_source_returns_platform_impl() {
    let source = create_capture_source();
    let targets = source.list_targets().await;
    assert!(targets.is_ok());
}

#[tokio::test]
async fn empty_targets_returns_error_on_windows() {
    #[cfg(target_os = "windows")]
    {
        let mut source = create_capture_source();
        let result = source.start(&[]).await;
        assert!(result.is_err());
    }
}
