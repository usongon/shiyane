//! Windows 音频采集编排：`WindowsCaptureSource` 组合三类音源
//! （`mic:{name}` 麦克风 / `system:all` 整机 loopback / `system:{pid,...}` 进程树
//! loopback），对外协议与 macOS 实现一致（`CaptureSource` trait）。

pub(crate) mod convert;
pub(crate) mod device;
pub(crate) mod process;
pub(crate) mod session;

use super::{CaptureKind, CaptureSource, CaptureTarget};
use crate::audio::AudioChunk;
use crate::{Error, Result};
use async_trait::async_trait;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use device::StreamHandle;

pub struct WindowsCaptureSource {
    streams: Vec<StreamHandle>,
}

impl WindowsCaptureSource {
    pub fn new() -> Self {
        Self { streams: Vec::new() }
    }
}

impl Default for WindowsCaptureSource {
    fn default() -> Self {
        Self::new()
    }
}

/// 每线程一次的 COM 初始化守卫：获取时调 CoInitializeEx(MTA)，Drop 调 CoUninitialize。
/// 两个获取变体：
/// - [`ComGuard::new`]（宽容）：线程已按其他并发模型初始化（RPC_E_CHANGED_MODE）时
///   沿用现状，不接管也不配对反初始化——适用于线程内自用、不向其他线程传递 COM
///   接口的场景（如采集线程，新线程首次初始化必然 MTA 成功），以及
///   `enumerate_audio_sessions` 这类同线程 STA 也合法的枚举入口。
/// - [`ComGuard::acquire_mta`]（严格）：遇 RPC_E_CHANGED_MODE 返回 Err——音频捕获的
///   调用线程侧序列（端点解析 → Activate → Initialize → Start）必须运行在 MTA，
///   否则裸接口移交 MTA 采集线程不安全（MtaInterface 的 SAFETY 前提）。
pub(crate) struct ComGuard {
    uninitialize: bool,
}

impl ComGuard {
    pub(crate) fn new() -> windows::core::Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self { uninitialize: true })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Self { uninitialize: false })
        } else {
            Err(hr.into())
        }
    }

    /// 严格 MTA 获取：本线程已是其他 apartment（STA 等，RPC_E_CHANGED_MODE）时返回
    /// Err 而非沿用现状。音频捕获入口（spawn_* / list_*）在调用线程侧必须用它，
    /// 使「创建线程在 MTA」成为确定性前提而非注释约定。
    pub(crate) fn acquire_mta() -> windows::core::Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self { uninitialize: true })
        } else {
            Err(hr.into())
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

#[async_trait]
impl CaptureSource for WindowsCaptureSource {
    async fn start(&mut self, target_ids: &[String]) -> Result<mpsc::Receiver<AudioChunk>> {
        if target_ids.is_empty() {
            return Err(Error::AudioSource("未选择任何音源".to_string()));
        }
        // 公开入口线程先立宽容 MTA 守卫；各 spawn_* 入口各自再做严格 acquire_mta，
        // 嵌套 CoInitializeEx 按线程引用计数配对，Drop 平衡
        let _com = ComGuard::new()
            .map_err(|e| Error::AudioSource(format!("CoInitializeEx failed: {e}")))?;
        let (tx, rx) = mpsc::channel::<AudioChunk>(64);
        let counter = Arc::new(AtomicI64::new(0));

        for id in target_ids {
            if let Some(name) = id.strip_prefix("mic:") {
                self.streams
                    .push(device::spawn_microphone(name, tx.clone(), counter.clone())?);
            } else if id == "system:all" {
                self.streams
                    .push(device::spawn_device_loopback(tx.clone(), counter.clone())?);
            } else if let Some(pids) = id.strip_prefix("system:") {
                // 同名合并组（system:100,200）按 pid 拆成多条流，共用同一 tx/counter
                for pid in pids.split(',') {
                    let pid: u32 = pid
                        .parse()
                        .map_err(|_| Error::AudioSource(format!("非法音源 id: {id}")))?;
                    self.streams.push(process::spawn_process_loopback(
                        pid,
                        tx.clone(),
                        counter.clone(),
                    )?);
                }
            } else {
                return Err(Error::AudioSource(format!("无法识别的音源 id: {id}")));
            }
        }
        Ok(rx)
    }

    async fn stop(&mut self) -> Result<()> {
        // join 是 async 上下文里的阻塞调用：数量少（每音源一条线程）且 stop 为低频
        // 操作，可接受；必须 join 而非 detach——确保采集线程（WASAPI Stop + COM
        // 反初始化收尾）确定完成后本方法才返回
        for s in self.streams.drain(..) {
            s.stop.store(true, Ordering::Relaxed);
            if let Some(Err(e)) = s.join.map(|j| j.join()) {
                tracing::warn!("采集线程 join 失败: {e:?}");
            }
        }
        Ok(())
    }

    async fn list_targets(&self) -> Result<Vec<CaptureTarget>> {
        // 公开入口线程宽容 MTA 守卫；list_microphone_targets 内部另做严格获取，
        // enumerate_audio_sessions 用宽容获取（同线程 STA 也合法）
        let _com = ComGuard::new()
            .map_err(|e| Error::AudioSource(format!("CoInitializeEx failed: {e}")))?;
        let mut targets = device::list_microphone_targets()?;
        targets.push(CaptureTarget {
            id: "system:all".to_string(),
            name: "系统音频（全部）".to_string(),
            kind: CaptureKind::SystemAudio,
            icon_path: None,
        });
        let sessions = session::enumerate_audio_sessions()?;
        targets.extend(super::build_system_targets(sessions, std::process::id()));
        Ok(targets)
    }
}
