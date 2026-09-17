use super::{CaptureKind, CaptureSource, CaptureTarget};
use crate::audio::AudioChunk;
use crate::{Error, Result};
use async_trait::async_trait;
use cpal::traits::HostTrait;
use tokio::sync::mpsc;

pub struct MacOSCaptureSource;

impl MacOSCaptureSource {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CaptureSource for MacOSCaptureSource {
    async fn start(&mut self, _target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>> {
        // TODO: 实现 ScreenCaptureKit 系统音频捕获 + cpal 麦克风捕获
        // 1. 对 target_ids 中的 system_audio 目标，创建 SCStream
        // 2. 对 target_ids 中的 microphone 目标，创建 cpal input stream
        // 3. 多源 PCM 数据通过 mpsc::channel 汇入统一 stream
        // 4. 用 rubato 重采样到 16kHz mono i16
        Err(Error::AudioSource(
            "macOS audio capture not yet implemented".to_string(),
        ))
    }

    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    async fn list_targets(&self) -> Result<Vec<CaptureTarget>> {
        let mut targets = Vec::new();

        // 列出麦克风（cpal）
        let host = cpal::default_host();
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                // cpal 0.18 移除了 Device::name()，使用 Display 实现获取设备名
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

        // 列出系统音频进程（ScreenCaptureKit）
        // TODO: 用 SCShareableContent 枚举可捕获的进程
        // 临时：返回全系统音频选项
        targets.push(CaptureTarget {
            id: "system:all".to_string(),
            name: "系统音频（全部）".to_string(),
            kind: CaptureKind::SystemAudio,
            icon_path: None,
        });

        Ok(targets)
    }
}
