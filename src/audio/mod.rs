pub mod capture;
pub mod file;

pub use file::FileAudioSource;

use crate::Result;
use async_trait::async_trait;
use std::time::Duration;

/// A chunk of audio data with associated timestamps.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Raw PCM audio samples (16-bit signed, little-endian).
    pub pcm: Vec<i16>,
    /// Timestamp in milliseconds relative to the source content timeline.
    pub content_time_ms: i64,
    /// Timestamp in milliseconds relative to wall-clock time.
    pub wall_time_ms: i64,
}

/// Trait for audio input sources.
///
/// Implementations provide audio data in chunks, optionally supporting
/// seek and reporting total duration.
#[async_trait]
pub trait AudioSource: Send + Sync {
    /// Fetch the next chunk of audio data.
    ///
    /// Returns `Err(Error::AudioSource("End of file"))` when the source is exhausted.
    async fn next_chunk(&mut self) -> Result<AudioChunk>;

    /// Seek to a specific position in the source.
    ///
    /// Only available if `supports_seek()` returns `true`.
    async fn seek(&mut self, pos: Duration) -> Result<()>;

    /// Whether this source supports seeking.
    fn supports_seek(&self) -> bool;

    /// Total duration of the source, if known.
    fn total_duration(&self) -> Option<Duration>;

    /// Extract complete audio track to a temporary WAV file.
    /// Returns the path to the temporary file.
    async fn extract_full_audio_to_wav(&self) -> Result<std::path::PathBuf>;
}
