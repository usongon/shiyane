use pick_up_sound_text::checkpoint::{
    Checkpoint, CheckpointFingerprint, SegmentProgress, SegmentStatus, compute_task_id,
};
use std::io::Write;
use std::path::{Path, PathBuf};

fn fp(source_language: &str, target_lang: &str) -> CheckpointFingerprint {
    CheckpointFingerprint {
        source_language: source_language.to_string(),
        target_lang: target_lang.to_string(),
        translate_provider: "openai".to_string(),
        translate_model: "gpt-3.5-turbo".to_string(),
        asr_model: "qwen-audio-3.0-asr-flash-filetrans".to_string(),
    }
}

fn pending(id: usize, source: &str) -> SegmentProgress {
    SegmentProgress {
        segment_id: id,
        start_time: Some(id as f64),
        end_time: Some(id as f64 + 1.0),
        status: SegmentStatus::Pending,
        source: Some(source.to_string()),
        translated: None,
        fallback: false,
    }
}

fn completed_update(id: usize, translated: &str) -> SegmentProgress {
    SegmentProgress {
        segment_id: id,
        start_time: None,
        end_time: None,
        status: SegmentStatus::Completed,
        source: None,
        translated: Some(translated.to_string()),
        fallback: false,
    }
}

#[test]
fn test_save_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");

    let mut cp = Checkpoint::new("t1".into(), PathBuf::from("/v/movie.mp4"), fp("auto", "zh"));
    cp.segments = vec![pending(0, "konnichiwa"), pending(1, "sayonara")];
    cp.segments[0].status = SegmentStatus::Completed;
    cp.segments[0].translated = Some("你好".into());
    cp.save(&path).unwrap();

    let loaded = Checkpoint::load(&path).unwrap().unwrap();
    assert_eq!(loaded.task_id, "t1");
    assert_eq!(loaded.video_path, PathBuf::from("/v/movie.mp4"));
    assert_eq!(loaded.fingerprint, fp("auto", "zh"));
    assert_eq!(loaded.segments.len(), 2);
    assert_eq!(loaded.segments[0].status, SegmentStatus::Completed);
    assert_eq!(loaded.segments[0].translated.as_deref(), Some("你好"));
    assert_eq!(loaded.segments[0].source.as_deref(), Some("konnichiwa"));
    assert_eq!(loaded.segments[1].status, SegmentStatus::Pending);
}

#[test]
fn test_fold_update_line_overrides_pending_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    // meta 行（指纹嵌套对象）
    writeln!(
        f,
        r#"{{"task_id":"t1","video_path":"/v/m.mp4","fingerprint":{}}}"#,
        r#"{"source_language":"auto","target_lang":"zh","translate_provider":"openai","translate_model":"gpt","asr_model":"qwen"}"#
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::to_string(&pending(0, "hello")).unwrap()
    )
    .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::to_string(&completed_update(0, "你好")).unwrap()
    )
    .unwrap();
    drop(f);

    let loaded = Checkpoint::load(&path).unwrap().unwrap();
    assert_eq!(loaded.segments.len(), 1); // 折叠为一行
    let seg = &loaded.segments[0];
    assert_eq!(seg.status, SegmentStatus::Completed);
    assert_eq!(seg.translated.as_deref(), Some("你好")); // 更新行提供
    assert_eq!(seg.source.as_deref(), Some("hello")); // 首次出现提供
    assert_eq!(seg.start_time, Some(0.0)); // 首次出现提供
}

#[test]
fn test_corrupt_tail_line_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"task_id":"t1","video_path":"/v/m.mp4","fingerprint":{}}}"#,
        r#"{"source_language":"auto","target_lang":"zh","translate_provider":"","translate_model":"","asr_model":""}"#
    )
    .unwrap();
    writeln!(f, "{}", serde_json::to_string(&pending(0, "ok")).unwrap()).unwrap();
    writeln!(f, "{{{{torn write").unwrap(); // 崩溃撕裂的半行
    drop(f);

    let loaded = Checkpoint::load(&path).unwrap().unwrap();
    assert_eq!(loaded.segments.len(), 1);
    assert_eq!(loaded.segments[0].source.as_deref(), Some("ok"));
    assert!(path.exists()); // 未被改名
}

#[test]
fn test_corrupt_meta_archived_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");
    std::fs::write(&path, "not json at all\n").unwrap();

    let loaded = Checkpoint::load(&path).unwrap();
    assert!(loaded.is_none()); // 当作无 checkpoint
    assert!(!path.exists()); // 原文件已改名
    assert!(dir.path().join("progress.jsonl.corrupt").exists());
}

#[test]
fn test_save_compacts_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"task_id":"t1","video_path":"/v/m.mp4","fingerprint":{}}}"#,
        r#"{"source_language":"auto","target_lang":"zh","translate_provider":"","translate_model":"","asr_model":""}"#
    )
    .unwrap();
    writeln!(f, "{}", serde_json::to_string(&pending(0, "a")).unwrap()).unwrap();
    writeln!(
        f,
        "{}",
        serde_json::to_string(&completed_update(0, "A")).unwrap()
    )
    .unwrap();
    drop(f);

    let cp = Checkpoint::load(&path).unwrap().unwrap();
    cp.save(&path).unwrap(); // 压缩重写

    let content = std::fs::read_to_string(&path).unwrap();
    let n = content.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(n, 2); // meta + 1 句终态
}

#[test]
fn test_helpers() {
    let mut cp = Checkpoint::new("t".into(), PathBuf::from("/v/m.mp4"), fp("auto", "zh"));
    assert!(!cp.is_all_completed()); // 空 segments
    assert_eq!(cp.percent(), 0.0);
    cp.segments = vec![pending(0, "a"), pending(1, "b")];
    cp.segments[0].status = SegmentStatus::Completed;
    cp.segments[0].translated = Some("A".into());
    assert_eq!(cp.completed_count(), 1);
    assert!(!cp.is_all_completed());
    assert!((cp.percent() - 0.5).abs() < 1e-9);
    cp.segments[1].status = SegmentStatus::Completed;
    cp.segments[1].translated = Some("B".into());
    assert!(cp.is_all_completed());
    assert!((cp.percent() - 1.0).abs() < 1e-9);
}

#[test]
fn test_task_id_stable_and_sensitive() -> std::io::Result<()> {
    use std::time::{Duration, SystemTime};

    let dir = tempfile::tempdir()?;
    let p = dir.path().join("video.mp4");
    std::fs::write(&p, vec![0u8; 100])?;

    let id1 = compute_task_id(&p).unwrap();
    let id1b = compute_task_id(&p).unwrap();
    assert_eq!(id1, id1b, "同文件 task_id 必须稳定");

    std::fs::write(&p, vec![0u8; 101])?; // 大小变化
    let id2 = compute_task_id(&p).unwrap();
    assert_ne!(id1, id2, "文件内容变化 task_id 必须变化");

    // 仅 mtime 变化（大小不变）
    let f = std::fs::File::options().write(true).open(&p)?;
    f.set_times(
        std::fs::FileTimes::new().set_modified(
            SystemTime::now()
                .checked_add(Duration::from_secs(3600))
                .unwrap(),
        ),
    )?;
    drop(f);
    let id3 = compute_task_id(&p).unwrap();
    assert_ne!(id2, id3, "mtime 变化 task_id 必须变化");

    // 不存在的文件要有错误而不是 panic
    assert!(compute_task_id(Path::new("/nonexistent/file.mp4")).is_err());
    Ok(())
}

#[test]
fn test_append_updates_appends_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("progress.jsonl");
    let mut cp = Checkpoint::new("t".into(), PathBuf::from("/v/m.mp4"), fp("auto", "zh"));
    cp.segments = vec![pending(0, "a")];
    cp.save(&path).unwrap();

    Checkpoint::append_updates(&path, &[completed_update(0, "A")]).unwrap();
    let loaded = Checkpoint::load(&path).unwrap().unwrap();
    assert_eq!(loaded.segments[0].status, SegmentStatus::Completed);
    assert_eq!(loaded.segments[0].translated.as_deref(), Some("A"));
}
