//! 进程树 loopback 捕获（微软 ApplicationLoopback 官方样例移植）。
//!
//! `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, IAudioClient,
//! VT_BLOB 激活参数)` → 完成回调 `GetActivateResult` 取 IAudioClient →
//! `GetMixFormat`（进程 loopback 不可指定格式）→ `Initialize(SHARED,
//! LOOPBACK|EVENTCALLBACK, 0, 0, mix_format)` → 复用 Task 4 的
//! `start_audio_client_stream` 跑 event-driven 采集循环。
//! 每个 pid 一条线程（同名合并组由 Task 6 拆成多条 spawn）。

use super::device::{start_audio_client_stream, MixFormat, MtaInterface, StreamHandle};
use super::ComGuard;
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
    // 调用线程侧严格 MTA 守卫：贯穿异步激活 → GetMixFormat → Initialize → Start，
    // MtaInterface 的 SAFETY 前提（创建线程在 MTA）由此确定性保证
    // （STA 调用线程会得到 Err 而非带着 STA apartment 跑完激活）
    let _com = ComGuard::acquire_mta().map_err(|e| {
        Error::AudioSource(format!(
            "[{tag}] CoInitializeEx(MTA) 失败: {e}（音频捕获需在 MTA 线程初始化）"
        ))
    })?;

    // 1. 激活参数：AUDIOCLIENT_ACTIVATION_PARAMS 装进 VT_BLOB PropVariant。
    //    blob 必须 CoTaskMemAlloc 堆分配且此后不手动释放：真机（Win11 26200）
    //    实证栈 blob 在激活成功路径会触发系统侧 CoTaskMemFree（对栈指针做堆
    //    释放 → 0xC0000374 堆损坏，激活完成后即崩）；而失败路径不触碰 blob。
    //    MSDN 未记载 activateOptions 的所有权契约（官方 C++ 样例用栈，本机不
    //    成立）。取「分配后移交、永不释放」：单次启动泄漏 ≤ size_of 参数结构，
    //    用户手动启动采集为低频操作，可忽略；换来杜绝双重释放/UAF。
    let activation_params: AUDIOCLIENT_ACTIVATION_PARAMS = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let blob_data: *mut u8 = unsafe {
        let p = windows::Win32::System::Com::CoTaskMemAlloc(
            std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>(),
        );
        if p.is_null() {
            return Err(Error::AudioSource(format!("[{tag}] CoTaskMemAlloc 失败")));
        }
        std::ptr::copy_nonoverlapping(
            &activation_params as *const _ as *const u8,
            p.cast(),
            std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>(),
        );
        p.cast()
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
                        pBlobData: blob_data,
                    },
                },
            }),
        },
    };

    // 2. 完成回调：结果存槽位，Condvar 唤醒
    let done: ActivateSlot = Arc::new((Mutex::new(None), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivateHandler { done: done.clone() }.into();

    // 3. 异步激活。返回的 operation 须存活到激活完成（MSDN：Windows 持有
    //    completionHandler 引用直到 operation 完成且应用释放 operation）
    let operation = unsafe {
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
            drop(operation);
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
    let handle = start_audio_client_stream(
        tag,
        audio_client,
        mix_format,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        tx,
        counter,
    );
    drop(operation);
    handle
}
