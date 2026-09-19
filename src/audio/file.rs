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

/// ffmpeg/ffprobe 解析顺序：.app 内自带（externalBin）→ 常见安装目录 →
/// 裸名交给 PATH。GUI 启动的进程只拿 launchd 最小 PATH，finder 双击也必须可用
fn resolve_tool_in(bundled_dir: &Path, name: &str, extra_dirs: &[PathBuf]) -> PathBuf {
    let bundled = bundled_dir.join(name);
    if bundled.is_file() {
        return bundled;
    }
    for dir in extra_dirs {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

/// Windows 下可执行文件名必须带 .exe（Tauri sidecar 安装后剥三元组落盘 ffmpeg.exe）
fn tool_exe_name(name: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("{name}.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        name.to_string()
    }
}

fn resolve_tool(name: &str) -> PathBuf {
    #[cfg(target_os = "macos")]
    let extra_dirs: Vec<PathBuf> = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    #[cfg(not(target_os = "macos"))]
    let extra_dirs: Vec<PathBuf> = Vec::new(); // Windows：sidecar → PATH 两级即可
    let bundled_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    resolve_tool_in(&bundled_dir, &tool_exe_name(name), &extra_dirs)
}

/// 子进程启动失败的友好映射：NotFound 给出安装指引，其余透传原因
fn tool_spawn_error(tool: &str, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        #[cfg(target_os = "macos")]
        let hint = "未检测到 {tool}。请安装后重试：brew install ffmpeg".to_string();
        #[cfg(not(target_os = "macos"))]
        let hint = "未检测到 {tool}。应用自带的 ffmpeg 丢失，请重装本应用；或在系统 PATH 中安装 ffmpeg".to_string();
        Error::AudioSource(hint.replace("{tool}", tool))
    } else {
        Error::AudioSource(format!("{tool} 执行失败: {e}"))
    }
}

/// 统一的 ffmpeg/ffprobe 子进程入口。Windows 下必须 CREATE_NO_WINDOW：
/// 这些工具是控制台程序，GUI 应用直接 spawn 时系统会新分配控制台，
/// 表现为开始转字幕瞬间闪一个黑窗（真机 B4 验收发现）
fn tool_command(tool: &str) -> Command {
    let mut cmd = Command::new(resolve_tool(tool));
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
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

        let output = tool_command("ffprobe")
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
            .await
            .map_err(|e| tool_spawn_error("ffprobe", e))?;

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

        let output = tool_command("ffmpeg")
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

        let output = tool_command("ffmpeg")
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

    #[test]
    fn resolve_tool_prefers_bundled_then_common_dirs_then_path() {
        let bundled = tempfile::tempdir().unwrap();
        let homebrew = tempfile::tempdir().unwrap();
        let empty = tempfile::tempdir().unwrap();

        // bundle 内存在 → 直接用自带
        std::fs::write(bundled.path().join("ffmpeg"), b"x").unwrap();
        std::fs::write(homebrew.path().join("ffmpeg"), b"x").unwrap();
        assert_eq!(
            resolve_tool_in(bundled.path(), "ffmpeg", &[homebrew.path().to_path_buf()]),
            bundled.path().join("ffmpeg")
        );

        // bundle 内没有 → 常见安装目录
        assert_eq!(
            resolve_tool_in(empty.path(), "ffmpeg", &[homebrew.path().to_path_buf()]),
            homebrew.path().join("ffmpeg")
        );

        // 都没有 → 裸名（交给 PATH）
        assert_eq!(
            resolve_tool_in(empty.path(), "ffmpeg", &[empty.path().to_path_buf()]),
            PathBuf::from("ffmpeg")
        );
    }

    #[test]
    fn bundled_tool_name_has_exe_suffix_on_windows() {
        // 跨平台断言：windows 期望带 .exe，mac 期望裸名
        let expected = if cfg!(target_os = "windows") {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        assert_eq!(tool_exe_name("ffmpeg"), expected);
    }
}
