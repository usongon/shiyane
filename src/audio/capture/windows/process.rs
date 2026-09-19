//! 进程树 loopback 捕获（微软 ApplicationLoopback 官方样例移植）。
//!
//! `ActivateAudioInterfaceAsync(VAD\Process_Loopback, IAudioClient, VT_BLOB
//! 激活参数)` → 完成回调 `GetActivateResult` 取 IAudioClient → `GetMixFormat`
//! （进程 loopback 不可指定格式）→ `Initialize(SHARED, LOOPBACK|EVENTCALLBACK,
//! 0, 0, mix_format)` → 复用 Task 4 的 `start_audio_client_stream` 跑
//! event-driven 采集循环。每个 pid 一条线程（同名合并组由 Task 6 拆成多条 spawn）。

use super::device::{start_audio_client_stream, MixFormat, MtaInterface, StreamHandle};
use super::ComGuard;
use crate::audio::AudioChunk;
use crate::{Error, Result};
use std::mem::ManuallyDrop;
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
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
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
    //    两处真机踩坑存档（2026-09-19）：
    //    a) 设备路径必须用 VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK 常量（其值为
    //       "VAD\Process_Loopback"）。曾误把宏名当字面量传入（w!("VIRTUAL_AUDIO_
    //       DEVICE_PROCESS_LOOPBACK")）→ 系统找不到该设备 → 激活一律
    //       0x80070002（ERROR_FILE_NOT_FOUND）。
    //    b) windows-rs 的 PROPVARIANT 自带 Drop=PropVariantClear，会对 VT_BLOB 的
    //       pBlobData 做 CoTaskMemFree——栈 blob 即对栈指针做堆释放（0xC0000374/
    //       0xC0000409 崩溃）。官方 C++ 样例语义是「栈参数 + 无人 clear」，故用
    //       外层 ManuallyDrop 抑制 Drop（内层包 ManuallyDrop 无效，Drop 在外层）。
    let mut activation_params: AUDIOCLIENT_ACTIVATION_PARAMS = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let activate_options = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
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
    });

    // 2. 完成回调：结果存槽位，Condvar 唤醒
    let done: ActivateSlot = Arc::new((Mutex::new(None), Condvar::new()));
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivateHandler { done: done.clone() }.into();

    // 3. 异步激活。返回的 operation 须存活到激活完成（MSDN：Windows 持有
    //    completionHandler 引用直到 operation 完成且应用释放 operation）
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*activate_options as *const PROPVARIANT),
            &handler,
        )
    }
    .map_err(|e| Error::AudioSource(format!("[{tag}] ActivateAudioInterfaceAsync 失败: {e}")))?;

    // 等待回调（5s 超时兜底）。activation_params 与 activate_options 均在作用域内
    // 存活至等待结束——官方样例同构。
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

    // 4. 进程 loopback 的激活客户端不支持 GetMixFormat（真机实测 E_NOTIMPL）；
    //    官方 ApplicationLoopback 样例同样自备格式（16bit PCM/44.1k）。这里自备
    //    引擎原生格式 f32/2ch/48k——与采集循环的 f32 解读及共享模式引擎一致，
    //    声道合并与 48k→16k 重采样沿用既有链路。
    let mix_format = unsafe {
        let p = windows::Win32::System::Com::CoTaskMemAlloc(
            std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>(),
        ) as *mut windows::Win32::Media::Audio::WAVEFORMATEX;
        if p.is_null() {
            return Err(Error::AudioSource(format!("[{tag}] CoTaskMemAlloc(WAVEFORMATEX) 失败")));
        }
        *p = windows::Win32::Media::Audio::WAVEFORMATEX {
            wFormatTag: super::device::WAVE_FORMAT_IEEE_FLOAT,
            nChannels: 2,
            nSamplesPerSec: 48000,
            wBitsPerSample: 32,
            nBlockAlign: 8,
            nAvgBytesPerSec: 48000 * 8,
            cbSize: 0,
        };
        MixFormat(p)
    };

    // 5. Initialize(SHARED, LOOPBACK|EVENTCALLBACK, 0, 0) + 采集循环（与 Task 4 完全一致，
    //    sample 参数：两个 duration 均为 0）
    let handle = start_audio_client_stream(
        tag,
        audio_client,
        mix_format,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        0,
        true,
        tx,
        counter,
    );
    drop(operation);
    handle
}
