use pick_up_sound_text::audio::AudioChunk;

#[tokio::test]
async fn test_audio_chunk_creation() {
    let chunk = AudioChunk {
        pcm: vec![0; 1600],
        content_time_ms: 0,
        wall_time_ms: 0,
    };
    assert_eq!(chunk.pcm.len(), 1600);
    assert_eq!(chunk.content_time_ms, 0);
}
