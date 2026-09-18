//! 进程树 loopback 捕获（微软 ApplicationLoopback 官方样例移植）。
//!
//! `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, IAudioClient,
//! VT_BLOB 激活参数)` → 完成回调 `GetActivateResult` 取 IAudioClient →
//! `GetMixFormat`（进程 loopback 不可指定格式）→ `Initialize(SHARED,
//! LOOPBACK|EVENTCALLBACK, 0, 0, mix_format)` → 复用 Task 4 的
//! `start_audio_client_stream` 跑 event-driven 采集循环。
//! 每个 pid 一条线程（同名合并组由 Task 6 拆成多条 spawn）。

use super::device::{start_audio_client_stream, MixFormat, MtaInterface, StreamHandle};
use super::session::ComGuard;
use crate::audio::AudioChunk;
use crate::{Error, Result};
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use windows::core::{implement, HRESULT, Interface, IUnknown, Ref};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioClient,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
};
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Variant::VT_BLOB;

/// 激活结果：MtaInterface 使 !Send 的接口包装可跨回调线程/等待线程传递（二者均为 MTA）
type ActivateSlot = Arc<(Mutex<Option<std::result::Result<MtaInterface<IAudioClient>, Error>>>, Condvar)>;

/// IActivateAudioInterfaceCompletionHandler：ActivateCompleted 里取
/// GetActivateResult 的 HRESULT + IUnknown，存入槽位并唤醒等待方。
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivateHandler {
    done: ActivateSlot,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateHandler_Impl {
    fn ActivateCompleted(
        &self,
        operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let result = match operation.ok() {
            Err(e) => Err(Error::AudioSource(format!("ActivateCompleted 收到空 operation: {e}"))),
            Ok(op) => unsafe {
                let mut activate_hr = HRESULT(0);
                let mut activated: Option<IUnknown> = None;
                match op.GetActivateResult(&mut activate_hr, &mut activated) {
                    Err(e) => Err(Error::AudioSource(format!("GetActivateResult 失败: {e}"))),
                    Ok(()) if activate_hr.is_ok() => {
                        match activated.and_then(|u| u.cast::<IAudioClient>().ok()) {
                            Some(client) => Ok(MtaInterface(client)),
                            None => Err(Error::AudioSource(format!(
                                "激活成功（hr={:#010x}）但未取得 IAudioClient",
                                activate_hr.0
                            ))),
                        }
                    }
                    Ok(()) => Err(Error::AudioSource(format!(
                        "进程 loopback 激活失败: hr={:#010x}",
                        activate_hr.0
                    ))),
                }
            },
        };
        *self.done.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        self.done.1.notify_all();
        Ok(())
    }
}

pub(crate) fn spawn_process_loopback(
    pid: u32,
    tx: mpsc::Sender<AudioChunk>,
    counter: Arc<AtomicI64>,
) -> Result<StreamHandle> {
    let tag = format!("进程 {pid} loopback");
    let _com = ComGuard::new()
        .map_err(|e| Error::AudioSource(format!("[{tag}] CoInitializeEx failed: {e}")))?;

    // 1. 激活参数：AUDIOCLIENT_ACTIVATION_PARAMS 装进 VT_BLOB PropVariant
    //    （pBlobData 指向栈上结构——不可 PropVariantClear，否则会对栈指针 CoTaskMemFree）
    let mut activation_params: AUDIOCLIENT_ACTIVATION_PARAMS = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let propvariant: PROPVARIANT = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut activation_params as *mut _ as *mut u8,
                    },
                },
            }),
        },
    };

    // 2. 完成回调：结果存槽位，Condvar 唤醒
    let done: ActivateSlot = Arc::new((Mutex::new(None), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivateHandler { done: done.clone() }.into();

    // 3. 异步激活（参数在栈上，须在本次调用返回前保持存活——上二者均在作用域内）
    unsafe {
        ActivateAudioInterfaceAsync(
            windows::core::w!("VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK"),
            &IAudioClient::IID,
            Some(&propvariant as *const PROPVARIANT),
            &handler,
        )
    }
    .map_err(|e| Error::AudioSource(format!("[{tag}] ActivateAudioInterfaceAsync 失败: {e}")))?;

    // 等待回调（5s 超时兜底）
    let (lock, cvar) = &*done;
    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let (g, timeout) = cvar
            .wait_timeout_while(guard, Duration::from_secs(5), |slot| slot.is_none())
            .unwrap_or_else(|e| e.into_inner());
        guard = g;
        if guard.is_none() {
            return Err(Error::AudioSource(format!(
                "[{tag}] 激活超时（5s, timed_out={}）",
                timeout.timed_out()
            )));
        }
    }
    let MtaInterface(audio_client) = match guard.take() {
        Some(Ok(client)) => client,
        Some(Err(e)) => return Err(e),
        // wait_timeout_while 谓词为假才提前返回，理论上不可达
        None => return Err(Error::AudioSource(format!("[{tag}] 激活结果槽位为空"))),
    };

    // 4. GetMixFormat（进程 loopback 不可指定格式）
    let mix_format = MixFormat(
        unsafe { audio_client.GetMixFormat() }
            .map_err(|e| Error::AudioSource(format!("[{tag}] GetMixFormat 失败: {e}")))?,
    );

    // 5. Initialize(SHARED, LOOPBACK|EVENTCALLBACK, 0, 0) + 采集循环（与 Task 4 完全一致，
    //    sample 参数：两个 duration 均为 0）
    start_audio_client_stream(
        tag,
        audio_client,
        mix_format,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        tx,
        counter,
    )
}
