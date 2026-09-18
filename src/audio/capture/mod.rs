#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::audio::AudioChunk;
use crate::Result;
use async_trait::async_trait;
use rubato::Resampler;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// 目标采样率：16kHz mono i16（macOS/Windows 共享）
pub(crate) const TARGET_SAMPLE_RATE: u32 = 16000;

pub(crate) fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

pub(crate) fn i16_to_f32(sample: i16) -> f32 {
    sample as f32 / i16::MAX as f32
}

/// 重采样 f32 数据到 16kHz mono，输出 i16
pub(crate) fn resample_to_target(
    samples: &[f32],
    source_rate: u32,
    resampler: &mut Option<rubato::Async<f32>>,
) -> Vec<i16> {
    if source_rate == TARGET_SAMPLE_RATE {
        // 无需重采样，直接转换
        return samples.iter().map(|&s| f32_to_i16(s)).collect();
    }

    // 初始化重采样器（lazy）
    if resampler.is_none() {
        let params = rubato::SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: Some(0.95),
            oversampling_factor: 128,
            interpolation: rubato::SincInterpolationType::Cubic,
            window: rubato::WindowFunction::BlackmanHarris2,
        };
        match rubato::Async::new_sinc(
            TARGET_SAMPLE_RATE as f64 / source_rate as f64,
            2.0,
            &params,
            samples.len().max(1024),
            1,
            rubato::FixedAsync::Input,
        ) {
            Ok(r) => *resampler = Some(r),
            Err(e) => {
                tracing::error!("Failed to create resampler: {}", e);
                return Vec::new();
            }
        }
    }

    let resampler = resampler.as_mut().unwrap();

    // rubato 需要固定 chunk size 输入
    let chunk_size = resampler.input_frames_next();
    let mut output = Vec::new();

    for chunk in samples.chunks(chunk_size) {
        let input = rubato::audioadapter_buffers::direct::InterleavedSlice::new(
            chunk,
            1,
            chunk.len(),
        );
        let Ok(input) = input else {
            tracing::warn!("Failed to create input adapter");
            continue;
        };

        if chunk.len() < chunk_size {
            // 最后一个不完整的 chunk，用 partial_len 处理
            match resampler.process(
                &input,
                Some(&rubato::Indexing {
                    input_offset: 0,
                    output_offset: 0,
                    partial_len: Some(chunk.len()),
                    active_channels_mask: None,
                }),
            ) {
                Ok(result) => {
                    let data = result.take_data();
                    output.extend(data.iter().map(|&s| f32_to_i16(s)));
                }
                Err(e) => {
                    tracing::warn!("Resample error: {}", e);
                }
            }
        } else {
            match resampler.process(&input, None) {
                Ok(result) => {
                    let data = result.take_data();
                    output.extend(data.iter().map(|&s| f32_to_i16(s)));
                }
                Err(e) => {
                    tracing::warn!("Resample error: {}", e);
                }
            }
        }
    }

    output
}

/// 按显示名分组 pid，保持首次出现顺序（macOS/Windows 共享）
pub(crate) fn group_pids_by_name(apps: Vec<(String, i32)>) -> Vec<(String, Vec<i32>)> {
    let mut groups: Vec<(String, Vec<i32>)> = Vec::new();
    for (name, pid) in apps {
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, pids)) => pids.push(pid),
            None => groups.push((name, vec![pid])),
        }
    }
    groups
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub id: String,
    pub name: String,
    pub kind: CaptureKind,
    pub icon_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    SystemAudio,
    Microphone,
}

#[async_trait]
pub trait CaptureSource: Send + Sync {
    async fn start(&mut self, target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>>;
    async fn stop(&mut self) -> Result<()>;
    async fn list_targets(&self) -> Result<Vec<CaptureTarget>>;
}

pub fn create_capture_source() -> Box<dyn CaptureSource> {
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacOSCaptureSource::new());
    #[cfg(target_os = "windows")]
    return Box::new(windows::WindowsCaptureSource::new());
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    compile_error!("Unsupported platform for audio capture");
}

#[cfg(test)]
mod tests {
    use super::group_pids_by_name;

    #[test]
    fn merges_same_name_pids() {
        let groups = group_pids_by_name(vec![
            ("微信".to_string(), 2120),
            ("Chrome".to_string(), 300),
            ("微信".to_string(), 10804),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], ("微信".to_string(), vec![2120, 10804]));
        assert_eq!(groups[1], ("Chrome".to_string(), vec![300]));
    }
}
