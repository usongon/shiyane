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

/// 仅 macOS 采集链路消费（Windows 转码链路全程 f32，无 i16 输入）
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn i16_to_f32(sample: i16) -> f32 {
    sample as f32 / i16::MAX as f32
}

/// 交错多声道 f32 → 单声道 f32（逐帧平均）
/// 仅 Windows loopback 转码消费；mac 侧无生产者（仅单测引用）
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn mixdown_interleaved(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    samples
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// 重采样持久状态：FixedIn 采样器 + 跨调用输入累积缓冲
pub(crate) struct ResamplerState {
    resampler: Option<rubato::Async<f32>>,
    pending: Vec<f32>,
}

impl Default for ResamplerState {
    fn default() -> Self {
        Self {
            resampler: None,
            pending: Vec::new(),
        }
    }
}

/// 重采样 f32 数据到 16kHz mono，输出 i16。
/// rubato FixedIn 只接受整块 chunk_size 输入（partial 仅流末尾一次合法）；
/// Windows 事件驱动采集每 ~10ms 到 480 样本（远小于 1024），逐包以 partial
/// 输入调用会产出 2.13 倍样本量的坏数据（真机实测：5s 窗口 170666 样本、
/// 有效采样率 34kHz）。故跨调用累积输入，凑满整块才 process；残留
/// ≤ chunk_size 输入样本留待下次（流末尾最多丢 ~21ms，字幕场景可忽略）。
pub(crate) fn resample_to_target(
    samples: &[f32],
    source_rate: u32,
    state: &mut ResamplerState,
) -> Vec<i16> {
    if source_rate == TARGET_SAMPLE_RATE {
        // 无需重采样，直接转换
        return samples.iter().map(|&s| f32_to_i16(s)).collect();
    }

    // 初始化重采样器（lazy）
    if state.resampler.is_none() {
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
            1024,
            1,
            rubato::FixedAsync::Input,
        ) {
            Ok(r) => state.resampler = Some(r),
            Err(e) => {
                tracing::error!("Failed to create resampler: {}", e);
                return Vec::new();
            }
        }
    }

    let ResamplerState { resampler, pending } = state;
    let resampler = resampler.as_mut().unwrap();
    let chunk_size = resampler.input_frames_next();

    // 累积本次输入，凑满整块才喂 FixedIn
    pending.extend_from_slice(samples);
    let mut output = Vec::new();
    while pending.len() >= chunk_size {
        let rest = pending.split_off(chunk_size);
        let chunk = std::mem::replace(pending, rest);
        let input = rubato::audioadapter_buffers::direct::InterleavedSlice::new(&chunk, 1, chunk.len());
        let Ok(input) = input else {
            tracing::warn!("Failed to create input adapter");
            continue;
        };
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

/// Windows 系统噪音进程（小写 exe 名）；真机实测后校准（Task 13）
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) const WINDOWS_NOISE_EXES: &[&str] = &[
    "audiodg.exe",
    "svchost.exe",
    "csrss.exe",
    "dwm.exe",
    "runtimebroker.exe",
    "applicationframehost.exe",
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "sihost.exe",
    "taskhostw.exe",
    "ctfmon.exe",
    "conhost.exe",
    "fontdrvhost.exe",
    "wudfhost.exe",
];

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn is_noise_session(exe_lower: &str, pid: u32, own_pid: u32) -> bool {
    pid == own_pid || WINDOWS_NOISE_EXES.iter().any(|n| *n == exe_lower)
}

/// "chrome.exe" → "Chrome"（去 .exe 后首字母大写）
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn friendly_name_from_exe(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut chars = stem.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// (pid, exe 名) 会话列表 → 系统音源条目：噪音/自身过滤 + 同名合并（id=system:{pid,...}）
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn build_system_targets(sessions: Vec<(u32, String)>, own_pid: u32) -> Vec<CaptureTarget> {
    let kept: Vec<(String, i32)> = sessions
        .into_iter()
        .filter_map(|(pid, exe)| {
            let name = friendly_name_from_exe(&exe);
            if name.is_empty() || is_noise_session(&exe.to_lowercase(), pid, own_pid) {
                return None;
            }
            Some((name, pid as i32))
        })
        .collect();
    group_pids_by_name(kept)
        .into_iter()
        .map(|(name, pids)| {
            let id = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
            CaptureTarget {
                id: format!("system:{}", id),
                name,
                kind: CaptureKind::SystemAudio,
                icon_path: None,
            }
        })
        .collect()
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

    use super::mixdown_interleaved;

    #[test]
    fn mixdown_stereo_averages_channels() {
        // L=1.0, R=-1.0 → 0.0；L=0.5, R=0.25 → 0.375
        assert_eq!(mixdown_interleaved(&[1.0, -1.0, 0.5, 0.25], 2), vec![0.0, 0.375]);
    }

    #[test]
    fn mixdown_mono_passthrough() {
        assert_eq!(mixdown_interleaved(&[0.25, -0.75], 1), vec![0.25, -0.75]);
    }

    use super::{build_system_targets, friendly_name_from_exe, is_noise_session};

    const OWN_PID: u32 = 42;

    #[test]
    fn noise_exes_filtered() {
        assert!(is_noise_session("audiodg.exe", 1, OWN_PID));
        assert!(is_noise_session("svchost.exe", 2, OWN_PID));
        assert!(!is_noise_session("chrome.exe", 3, OWN_PID));
    }

    #[test]
    fn own_pid_filtered() {
        assert!(is_noise_session("shiyane.exe", OWN_PID, OWN_PID));
    }

    #[test]
    fn exe_name_to_friendly() {
        assert_eq!(friendly_name_from_exe("chrome.exe"), "Chrome");
        assert_eq!(friendly_name_from_exe("cloudmusic.exe"), "Cloudmusic");
        assert_eq!(friendly_name_from_exe("QQ"), "QQ");
    }

    #[test]
    fn build_targets_merges_and_filters() {
        let targets = build_system_targets(
            vec![
                (100, "chrome.exe".to_string()),
                (200, "chrome.exe".to_string()),
                (300, "audiodg.exe".to_string()),
                (OWN_PID, "shiyane.exe".to_string()),
            ],
            OWN_PID,
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id, "system:100,200");
        assert_eq!(targets[0].name, "Chrome");
    }
}
