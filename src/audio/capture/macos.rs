use super::{CaptureKind, CaptureSource, CaptureTarget};
use super::{f32_to_i16, group_pids_by_name, i16_to_f32, resample_to_target, TARGET_SAMPLE_RATE};
use crate::audio::AudioChunk;
use crate::{Error, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 内部持有的活跃 stream 句柄，用于 stop() 时停止
enum ActiveStream {
    #[cfg(target_os = "macos")]
    ScreenCaptureKit(screencapturekit::prelude::SCStream),
    Cpal(cpal::Stream),
}

pub struct MacOSCaptureSource {
    streams: Vec<ActiveStream>,
    /// 用于生成单调递增的 content_time_ms
    content_time_counter: Arc<AtomicI64>,
}

impl Default for MacOSCaptureSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MacOSCaptureSource {
    pub fn new() -> Self {
        Self {
            streams: Vec::new(),
            content_time_counter: Arc::new(AtomicI64::new(0)),
        }
    }

    /// 启动系统音频采集（ScreenCaptureKit）
    #[cfg(target_os = "macos")]
    async fn start_system_audio(
        &mut self,
        target_id: &str,
        tx: mpsc::Sender<AudioChunk>,
    ) -> Result<()> {
        use screencapturekit::prelude::*;

        // 获取可共享内容
        let content = SCShareableContent::get()
            .map_err(|e| {
                Error::AudioSource(format!(
                    "无法获取屏幕共享内容（需要「屏幕录制」权限）: {}。请在 系统设置 > 隐私与安全性 > 屏幕录制 中授权本应用。",
                    e
                ))
            })?;

        let displays = content.displays();
        if displays.is_empty() {
            return Err(Error::AudioSource("没有可用的显示器".to_string()));
        }

        // 创建内容过滤器
        let filter = if target_id == "system:all" {
            // 全系统音频：捕获整个显示器
            SCContentFilter::create()
                .with_display(&displays[0])
                .with_excluding_windows(&[])
                .build()
        } else if let Some(pid_part) = target_id.strip_prefix("system:") {
            // 特定进程音频（同名进程合并后可能携带多个 pid）
            let pids: Vec<i32> = pid_part
                .split(',')
                .map(|s| s.parse::<i32>())
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| Error::AudioSource(format!("无效的进程 ID: {}", target_id)))?;

            let apps = content.applications();
            let matched: Vec<_> = apps
                .iter()
                .filter(|a| pids.contains(&a.process_id()))
                .collect();
            if matched.is_empty() {
                return Err(Error::AudioSource(format!(
                    "找不到进程 ID 为 {} 的应用",
                    pid_part
                )));
            }

            SCContentFilter::create()
                .with_display(&displays[0])
                .with_including_applications(&matched, &[])
                .build()
        } else {
            return Err(Error::AudioSource(format!(
                "未知的系统音频目标: {}",
                target_id
            )));
        };

        // 配置音频参数
        let config = SCStreamConfiguration::new()
            .with_captures_audio(true)
            .with_sample_rate(TARGET_SAMPLE_RATE as i32)
            .with_channel_count(1);

        // 创建 stream
        let mut stream = SCStream::new(&filter, &config);

        let content_time_counter = self.content_time_counter.clone();

        // 添加音频输出处理器
        let handler = move |sample_buffer: CMSampleBuffer, of_type: SCStreamOutputType| {
            if of_type != SCStreamOutputType::Audio {
                return;
            }

            // 提取音频数据
            let Some(audio_list) = sample_buffer.audio_buffer_list() else {
                return;
            };

            let mut pcm_data = Vec::new();
            for buffer in audio_list.iter() {
                let data = buffer.data();
                // ScreenCaptureKit 输出的是 f32 格式
                let samples: &[f32] = unsafe {
                    std::slice::from_raw_parts(
                        data.as_ptr() as *const f32,
                        data.len() / std::mem::size_of::<f32>(),
                    )
                };
                pcm_data.extend_from_slice(samples);
            }

            if pcm_data.is_empty() {
                return;
            }

            // 转换为 i16（配置已设为 16kHz mono，无需重采样）
            let pcm: Vec<i16> = pcm_data.iter().map(|&s| f32_to_i16(s)).collect();

            let content_time = content_time_counter.fetch_add(pcm.len() as i64, Ordering::SeqCst);
            let wall_time = chrono::Utc::now().timestamp_millis();

            let chunk = AudioChunk {
                pcm,
                content_time_ms: content_time / (TARGET_SAMPLE_RATE as i64 / 1000),
                wall_time_ms: wall_time,
            };

            if tx.blocking_send(chunk).is_err() {
                // channel 已关闭，忽略
            }
        };

        stream.add_output_handler(handler, SCStreamOutputType::Audio);

        stream.start_capture().map_err(|e| {
            Error::AudioSource(format!("启动系统音频采集失败: {}", e))
        })?;

        self.streams.push(ActiveStream::ScreenCaptureKit(stream));

        tracing::info!("ScreenCaptureKit 系统音频采集已启动: {}", target_id);
        Ok(())
    }

    /// 启动麦克风采集（cpal）
    fn start_microphone(
        &mut self,
        target_id: &str,
        tx: mpsc::Sender<AudioChunk>,
    ) -> Result<()> {
        let device_name = target_id.strip_prefix("mic:").ok_or_else(|| {
            Error::AudioSource(format!("无效的麦克风目标 ID: {}", target_id))
        })?;

        let host = cpal::default_host();

        // 查找指定名称的输入设备
        let device = host
            .input_devices()
            .map_err(|e| Error::AudioSource(format!("枚举输入设备失败: {}", e)))?
            .find(|d| d.to_string() == device_name)
            .ok_or_else(|| Error::AudioSource(format!("找不到麦克风: {}", device_name)))?;

        let config = device
            .default_input_config()
            .map_err(|e| Error::AudioSource(format!("获取输入配置失败: {}", e)))?;

        let sample_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let sample_format = config.sample_format();

        tracing::info!(
            "麦克风 {}: {}Hz, {} channels, {:?}",
            device_name,
            sample_rate,
            channels,
            sample_format
        );

        let content_time_counter = self.content_time_counter.clone();

        // 用于重采样的状态（在回调中共享）
        let resampler_state: Arc<Mutex<Option<rubato::Async<f32>>>> = Arc::new(Mutex::new(None));

        let stream = match sample_format {
            cpal::SampleFormat::I16 => {
                let resampler_clone = resampler_state.clone();
                let content_time_counter = content_time_counter.clone();
                device
                    .build_input_stream(
                        config.into(),
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            // 转为 f32，重采样，再转回 i16
                            let f32_data: Vec<f32> =
                                data.iter().map(|&s| i16_to_f32(s)).collect();

                            // 多声道转单声道
                            let mono: Vec<f32> = if channels > 1 {
                                f32_data
                                    .chunks(channels)
                                    .map(|c| c.iter().sum::<f32>() / channels as f32)
                                    .collect()
                            } else {
                                f32_data
                            };

                            let mut resampler_guard = resampler_clone.lock().unwrap();
                            let pcm =
                                resample_to_target(&mono, sample_rate, &mut *resampler_guard);
                            drop(resampler_guard);

                            if pcm.is_empty() {
                                return;
                            }

                            let content_time = content_time_counter
                                .fetch_add(pcm.len() as i64, Ordering::SeqCst);
                            let wall_time = chrono::Utc::now().timestamp_millis();

                            let chunk = AudioChunk {
                                pcm,
                                content_time_ms: content_time
                                    / (TARGET_SAMPLE_RATE as i64 / 1000),
                                wall_time_ms: wall_time,
                            };

                            if tx.blocking_send(chunk).is_err() {
                                // channel 已关闭
                            }
                        },
                        move |err| {
                            tracing::error!("麦克风采集错误: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| Error::AudioSource(format!("创建麦克风流失败: {}", e)))?
            }
            cpal::SampleFormat::F32 => {
                let resampler_clone = resampler_state.clone();
                device
                    .build_input_stream(
                        config.into(),
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            // 多声道转单声道
                            let mono: Vec<f32> = if channels > 1 {
                                data.chunks(channels)
                                    .map(|c| c.iter().sum::<f32>() / channels as f32)
                                    .collect()
                            } else {
                                data.to_vec()
                            };

                            let mut resampler_guard = resampler_clone.lock().unwrap();
                            let pcm =
                                resample_to_target(&mono, sample_rate, &mut *resampler_guard);
                            drop(resampler_guard);

                            if pcm.is_empty() {
                                return;
                            }

                            let content_time = content_time_counter
                                .fetch_add(pcm.len() as i64, Ordering::SeqCst);
                            let wall_time = chrono::Utc::now().timestamp_millis();

                            let chunk = AudioChunk {
                                pcm,
                                content_time_ms: content_time
                                    / (TARGET_SAMPLE_RATE as i64 / 1000),
                                wall_time_ms: wall_time,
                            };

                            if tx.blocking_send(chunk).is_err() {
                                // channel 已关闭
                            }
                        },
                        move |err| {
                            tracing::error!("麦克风采集错误: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| Error::AudioSource(format!("创建麦克风流失败: {}", e)))?
            }
            _ => {
                return Err(Error::AudioSource(format!(
                    "不支持的采样格式: {:?}",
                    sample_format
                )))
            }
        };

        stream
            .play()
            .map_err(|e| Error::AudioSource(format!("启动麦克风流失败: {}", e)))?;

        self.streams.push(ActiveStream::Cpal(stream));

        tracing::info!("麦克风采集已启动: {}", device_name);
        Ok(())
    }
}

#[async_trait]
impl CaptureSource for MacOSCaptureSource {
    async fn start(&mut self, target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>> {
        // 先停止之前的采集
        self.stop().await?;

        // 重置计数器
        self.content_time_counter.store(0, Ordering::SeqCst);

        let (tx, rx) = mpsc::channel::<AudioChunk>(256);

        if target_ids.is_empty() {
            // 默认：采集全系统音频
            #[cfg(target_os = "macos")]
            {
                self.start_system_audio("system:all", tx.clone()).await?;
            }
        } else {
            for target_id in target_ids {
                if target_id.starts_with("system:") {
                    #[cfg(target_os = "macos")]
                    {
                        self.start_system_audio(target_id, tx.clone()).await?;
                    }
                } else if target_id.starts_with("mic:") {
                    self.start_microphone(target_id, tx.clone())?;
                } else {
                    tracing::warn!("未知的采集目标: {}", target_id);
                }
            }
        }

        // 如果没有任何 stream 启动成功，返回错误
        if self.streams.is_empty() {
            return Err(Error::AudioSource(
                "没有成功启动任何音频采集源".to_string(),
            ));
        }

        Ok(rx)
    }

    async fn stop(&mut self) -> Result<()> {
        for stream in self.streams.drain(..) {
            match stream {
                #[cfg(target_os = "macos")]
                ActiveStream::ScreenCaptureKit(s) => {
                    if let Err(e) = s.stop_capture() {
                        tracing::warn!("停止 ScreenCaptureKit 流失败: {}", e);
                    }
                }
                ActiveStream::Cpal(s) => {
                    if let Err(e) = s.pause() {
                        tracing::warn!("停止 cpal 流失败: {}", e);
                    }
                }
            }
        }
        tracing::info!("所有音频采集已停止");
        Ok(())
    }

    async fn list_targets(&self) -> Result<Vec<CaptureTarget>> {
        let mut targets = Vec::new();

        // 列出麦克风（cpal）
        let host = cpal::default_host();
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let name = device.to_string();
                if name.is_empty() {
                    continue;
                }
                targets.push(CaptureTarget {
                    id: format!("mic:{}", name),
                    name: name.clone(),
                    kind: CaptureKind::Microphone,
                    icon_path: None,
                });
            }
        }

        // 列出系统音频（ScreenCaptureKit）
        targets.push(CaptureTarget {
            id: "system:all".to_string(),
            name: "系统音频（全部）".to_string(),
            kind: CaptureKind::SystemAudio,
            icon_path: None,
        });

        // 尝试枚举可捕获音频的进程
        #[cfg(target_os = "macos")]
        {
            if let Ok(content) = screencapturekit::prelude::SCShareableContent::get() {
                let own_pid = std::process::id() as i32;
                let mut filtered = 0;
                let kept: Vec<(String, i32)> = content
                    .applications()
                    .iter()
                    .filter_map(|app| {
                        let name = app.application_name();
                        let pid = app.process_id();
                        if name.is_empty() {
                            return None;
                        }
                        if is_noise_process(&name, &app.bundle_identifier(), pid, own_pid) {
                            filtered += 1;
                            return None;
                        }
                        Some((name, pid))
                    })
                    .collect();
                if filtered > 0 {
                    tracing::debug!("已隐藏 {} 个不发声的系统进程", filtered);
                }
                // 同名进程（如微信的内核/播放器辅助进程）合并为一个音源，
                // id 记录全部 pid，过滤器会一并包含
                for (name, pids) in group_pids_by_name(kept) {
                    if pids.len() > 1 {
                        tracing::debug!("同名进程合并为单音源: {} pids={:?}", name, pids);
                    }
                    let id = pids
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    targets.push(CaptureTarget {
                        // PID 只进 id（选择键），不进展示名——用户不关心
                        id: format!("system:{}", id),
                        name,
                        kind: CaptureKind::SystemAudio,
                        icon_path: None,
                    });
                }
            }
        }

        Ok(targets)
    }
}

/// 苹果自家会发声的应用白名单（bundle ID 前缀，小写比较）；
/// 其余 com.apple.*（通知中心、菜单栏 agent、系统设置等）不发声，隐藏
const APPLE_AUDIO_APPS: &[&str] = &[
    "com.apple.safari",
    "com.apple.music",
    "com.apple.podcasts",
    "com.apple.tv",
    "com.apple.quicktimeplayer",
    "com.apple.facetime",
    "com.apple.voicememos",
    "com.apple.maps",
    "com.apple.garageband",
    "com.apple.logic",
];

/// 本应用自身的 bundle ID
const OWN_BUNDLE_ID: &str = "com.usongon.shiyane";

/// 过滤 ScreenCaptureKit 枚举出的不发声进程：
/// - 「自动填充 (XX) / AutoFill(XX)」是各 App 的密码/表单填充辅助进程，从不发声
/// - 本应用自身
/// - com.apple.* 中不在发声白名单里的系统进程
fn is_noise_process(name: &str, bundle_id: &str, pid: i32, own_pid: i32) -> bool {
    let name_lower = name.trim().to_lowercase();
    if name.starts_with("自动填充") || name_lower.starts_with("autofill") {
        return true;
    }
    if pid == own_pid || bundle_id.eq_ignore_ascii_case(OWN_BUNDLE_ID) {
        return true;
    }
    let bundle_lower = bundle_id.to_ascii_lowercase();
    if !bundle_lower.starts_with("com.apple.") {
        return false;
    }
    !APPLE_AUDIO_APPS.iter().any(|b| bundle_lower.starts_with(b))
}

#[cfg(test)]
mod tests {
    use super::is_noise_process;

    const OWN_PID: i32 = 42;

    #[test]
    fn filters_autofill_helpers() {
        assert!(is_noise_process("自动填充 (Qoder)", "com.qoder.app", 1, OWN_PID));
        assert!(is_noise_process("自动填充 (微信)", "com.tencent.xinWeChat", 2, OWN_PID));
        assert!(is_noise_process("AutoFill(FlClash)", "com.follow.clash", 3, OWN_PID));
        assert!(is_noise_process("Autofill (Chrome)", "com.google.Chrome", 4, OWN_PID));
    }

    #[test]
    fn filters_self() {
        assert!(is_noise_process("拾言", "com.usongon.shiyane", OWN_PID, OWN_PID));
        assert!(is_noise_process("Shiyane Helper", "com.usongon.shiyane", 50, OWN_PID));
    }

    #[test]
    fn filters_apple_system_processes() {
        assert!(is_noise_process(
            "UserNotificationCenter",
            "com.apple.usernotifications.agent",
            7,
            OWN_PID
        ));
        assert!(is_noise_process("MenuBarAgent", "com.apple.menuagent", 8, OWN_PID));
        assert!(is_noise_process("CursorUIViewService", "com.apple.CursorUIService", 9, OWN_PID));
        assert!(is_noise_process("系统设置", "com.apple.systempreferences", 10, OWN_PID));
    }

    #[test]
    fn keeps_audio_capable_apps() {
        assert!(!is_noise_process("Safari", "com.apple.Safari", 11, OWN_PID));
        assert!(!is_noise_process("音乐", "com.apple.Music", 12, OWN_PID));
        assert!(!is_noise_process("QuickTime Player", "com.apple.QuickTimePlayer", 13, OWN_PID));
        assert!(!is_noise_process("GarageBand", "com.apple.garageband10", 14, OWN_PID));
    }

    #[test]
    fn keeps_third_party_apps() {
        assert!(!is_noise_process("微信", "com.tencent.xinWeChat", 15, OWN_PID));
        assert!(!is_noise_process("Google Chrome", "com.google.Chrome", 16, OWN_PID));
    }
}
