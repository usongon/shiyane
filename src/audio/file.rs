use crate::audio::{AudioChunk, AudioSource};
use crate::{Error, Result};
use async_trait::async_trait;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// 临时 wav 路径按视频路径确定性派生：暂停/崩溃留下的半截文件
/// 会在同视频下次提取时被 ffmpeg -y 直接覆盖，不在 /tmp 堆积
fn temp_wav_path(video_path: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    video_path.hash(&mut hasher);
    std::env::temp_dir().join(format!("shiyane-{:x}.wav", hasher.finish()))
}

/// Audio source that extracts audio from a video file using ffmpeg.
///
/// Segments the file into 10-minute chunks with 5-second overlap.
/// Timestamps are based on the video file's own timeline.
pub struct FileAudioSource {
    video_path: PathBuf,
    current_position: Duration,
    total_duration: Duration,
    segment_duration: Duration,
    overlap_duration: Duration,
}

impl FileAudioSource {
    /// Create a new `FileAudioSource` for the given video file.
    ///
    /// Uses `ffprobe` to determine the total duration of the file.
    pub async fn new(video_path: PathBuf) -> Result<Self> {
        let total_duration = Self::get_video_duration(&video_path).await?;
        Ok(Self {
            video_path,
            current_position: Duration::ZERO,
            total_duration,
            segment_duration: Duration::from_secs(600), // 10 minutes
            overlap_duration: Duration::from_secs(5),
        })
    }

    async fn get_video_duration(path: &PathBuf) -> Result<Duration> {
        let path_str = path
            .to_str()
            .ok_or_else(|| Error::AudioSource("Path contains invalid UTF-8".to_string()))?;

        let output = Command::new("ffprobe")
            .kill_on_drop(true)
            .args(&[
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
                path_str,
            ])
            .output()
            .await?;

        if !output.status.success() {
            return Err(Error::AudioSource(format!(
                "ffprobe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let duration_str = String::from_utf8_lossy(&output.stdout);
        let duration_secs: f64 = duration_str
            .trim()
            .parse()
            .map_err(|e| Error::AudioSource(format!("Failed to parse duration: {}", e)))?;

        Ok(Duration::from_secs_f64(duration_secs))
    }

    async fn extract_audio_segment(&self, start: Duration, duration: Duration) -> Result<Vec<i16>> {
        let path_str = self
            .video_path
            .to_str()
            .ok_or_else(|| Error::AudioSource("Path contains invalid UTF-8".to_string()))?;

        let output = Command::new("ffmpeg")
            .kill_on_drop(true)
            .args(&[
                "-ss",
                &format!("{:.3}", start.as_secs_f64()),
                "-i",
                path_str,
                "-t",
                &format!("{:.3}", duration.as_secs_f64()),
                "-vn",
                "-acodec",
                "pcm_s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
                "-f",
                "s16le",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await?;

        if !output.status.success() {
            return Err(Error::AudioSource("ffmpeg extraction failed".to_string()));
        }

        // Convert bytes to i16 samples
        let pcm: Vec<i16> = output
            .stdout
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        Ok(pcm)
    }
}

#[async_trait]
impl AudioSource for FileAudioSource {
    async fn next_chunk(&mut self) -> Result<AudioChunk> {
        if self.current_position >= self.total_duration {
            return Err(Error::AudioSource("End of file".to_string()));
        }

        let remaining = self.total_duration - self.current_position;
        let chunk_duration =
            std::cmp::min(self.segment_duration + self.overlap_duration, remaining);

        let pcm = self
            .extract_audio_segment(self.current_position, chunk_duration)
            .await?;

        let chunk = AudioChunk {
            pcm,
            content_time_ms: self.current_position.as_millis() as i64,
            wall_time_ms: self.current_position.as_millis() as i64,
        };

        // Move position forward by segment_duration, but not beyond total_duration
        self.current_position = std::cmp::min(
            self.current_position + self.segment_duration,
            self.total_duration,
        );

        Ok(chunk)
    }

    async fn seek(&mut self, pos: Duration) -> Result<()> {
        if pos > self.total_duration {
            return Err(Error::AudioSource(
                "Seek position beyond file duration".to_string(),
            ));
        }
        self.current_position = pos;
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        true
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(self.total_duration)
    }

    async fn extract_full_audio_to_wav(&self) -> Result<PathBuf> {
        let path_str = self
            .video_path
            .to_str()
            .ok_or_else(|| Error::AudioSource("Path contains invalid UTF-8".to_string()))?;

        // Create temp file path（确定性命名：暂停遗留的半截文件被 -y 覆盖）
        let temp_file = temp_wav_path(&self.video_path);

        let output = Command::new("ffmpeg")
            .kill_on_drop(true)
            .args(&[
                "-i",
                path_str,
                "-vn",
                "-acodec",
                "pcm_s16le",
                "-ar",
                "16000",
                "-ac",
                "1",
                "-y", // Overwrite if exists
                temp_file.to_str().ok_or_else(|| Error::AudioSource("Invalid temp path".to_string()))?,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await?;

        if !output.status.success() {
            return Err(Error::AudioSource("ffmpeg audio extraction failed".to_string()));
        }

        Ok(temp_file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_wav_path_is_deterministic_per_video() {
        let a = PathBuf::from("/movies/test.mp4");
        assert_eq!(temp_wav_path(&a), temp_wav_path(&a));
        assert_ne!(
            temp_wav_path(&a),
            temp_wav_path(&PathBuf::from("/movies/other.mp4"))
        );
        let p = temp_wav_path(&a);
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("wav"));
        assert!(p
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("shiyane-"));
    }
}
