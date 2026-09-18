//! 麦克风 + 整机（默认输出端点）loopback 采集线程。
//!
//! 结构约定：入口（`list_microphone_targets` / `spawn_device_loopback` /
//! `spawn_microphone`）在调用线程顶部以 `ComGuard::acquire_mta()` 严格获取 MTA 守卫，
//! 守卫贯穿端点解析、Activate、GetMixFormat、Initialize、事件句柄、Start 等可失败的
//! 同步步骤（失败直接返回 Err），保证这些 COM 调用全程有线程 apartment 且确定在 MTA；
//! Start 之后的 event-driven 采集循环放子线程（错误时 tracing::error 后退出，stop 置位
//! 后 ≤200ms 内收尾）。采集循环 `run_event_capture_loop` 与 `start_audio_client_stream`
//! 被 Task 5 进程树 loopback 复用。

use super::super::{CaptureKind, CaptureTarget};
use super::convert::frames_to_pcm_i16;
use super::session::ComGuard;
use crate::audio::AudioChunk;
use crate::{Error, Result};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, STGM_READ, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

/// 100ms 缓冲对应的 hns（100ns 单位）
const HNS_100MS: i64 = 100 * 10_000;

const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT：{00000003-0000-0010-8000-00aa00389b71}
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);

/// 活跃采集线程句柄（Task 5/6 复用）：stop 置位后线程退出，join 收尾
pub(crate) struct StreamHandle {
    pub stop: Arc<AtomicBool>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

/// GetMixFormat 返回的 WAVEFORMATEX*（CoTaskMem 分配）守卫
pub(crate) struct MixFormat(pub(crate) *mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0 as *const core::ffi::c_void)) };
    }
}

/// CreateEventW 句柄守卫
struct EventGuard(HANDLE);

impl Drop for EventGuard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: 事件句柄（内核对象）可跨线程等待/置位（内核语义保证）
unsafe impl Send for EventGuard {}

/// windows 0.61 起 COM 接口包装不再实现 Send。WASAPI 的 IAudioClient/IAudioCaptureClient
/// 在 MTA 内跨线程调用安全（创建线程经 `ComGuard::acquire_mta` 严格确认 MTA、采集线程
/// 以 COINIT_MULTITHREADED 自行初始化），故显式声明可跨线程传递。
pub(crate) struct MtaInterface<T>(pub(crate) T);

// SAFETY: 仅用于创建线程与采集线程均为 MTA 的 WASAPI 接口（见类型注释）
unsafe impl<T> Send for MtaInterface<T> {}

/// 调用线程侧严格 MTA COM 守卫：持有期间本线程确定处于 MTA（否则 Err），
/// 覆盖端点解析 → Activate → Initialize → Start 整个序列；Start 后接口经
/// MtaInterface 移交子线程，其 SAFETY 前提（创建线程在 MTA）由本守卫确定性保证。
fn caller_thread_com(tag: &str) -> Result<ComGuard> {
    ComGuard::acquire_mta().map_err(|e| {
        Error::AudioSource(format!(
            "[{tag}] CoInitializeEx(MTA) 失败: {e}（音频捕获需在 MTA 线程初始化）"
        ))
    })
}

/// CoCreateInstance MMDeviceEnumerator。`_com` 参数是调用线程 ComGuard 存活的
/// 编译期证明——本函数不得在无守卫作用域内调用（COM 调用须有 apartment）。
fn device_enumerator(_com: &ComGuard) -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| Error::AudioSource(format!("CoCreateInstance(MMDeviceEnumerator) failed: {e}")))
}

/// 端点 FriendlyName（PKEY_Device_FriendlyName，VT_LPWSTR；其他类型返回空串）
fn endpoint_friendly_name(device: &IMMDevice) -> Result<String> {
    unsafe {
        let store: IPropertyStore = device
            .OpenPropertyStore(STGM_READ)
            .map_err(|e| Error::AudioSource(format!("OpenPropertyStore failed: {e}")))?;
        let mut prop: PROPVARIANT = store
            .GetValue(&PKEY_Device_FriendlyName)
            .map_err(|e| Error::AudioSource(format!("GetValue(PKEY_Device_FriendlyName) failed: {e}")))?;
        let name = if prop.Anonymous.Anonymous.vt == VT_LPWSTR {
            let pwsz = prop.Anonymous.Anonymous.Anonymous.pwszVal;
            if !pwsz.is_null() {
                pwsz.to_string().unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let _ = PropVariantClear(&mut prop);
        Ok(name)
    }
}

/// 枚举活跃采集端点 → (设备, FriendlyName) 列表。
/// `_com` 参数要求调用线程 ComGuard 存活（EnumAudioEndpoints/OpenPropertyStore/GetValue
/// 均为 COM 调用）。
fn capture_endpoints(_com: &ComGuard) -> Result<Vec<(IMMDevice, String)>> {
    let enumerator = device_enumerator(_com)?;
    let collection = unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }
        .map_err(|e| Error::AudioSource(format!("EnumAudioEndpoints(eCapture) failed: {e}")))?;
    let count = unsafe { collection.GetCount() }
        .map_err(|e| Error::AudioSource(format!("GetCount failed: {e}")))?;
    let mut endpoints = Vec::new();
    for i in 0..count {
        let Ok(device) = (unsafe { collection.Item(i) }) else {
            continue;
        };
        match endpoint_friendly_name(&device) {
            Ok(name) => endpoints.push((device, name)),
            Err(e) => tracing::warn!("读取设备 {} FriendlyName 失败: {e}", i),
        }
    }
    Ok(endpoints)
}

/// 共享模式 mix format 必须是 f32（GetBuffer 按 f32 交错帧解释）
/// # Safety
/// `wfx` 须指向 GetMixFormat 返回的有效 WAVEFORMATEX
unsafe fn mix_format_is_float(wfx: *const WAVEFORMATEX) -> bool {
    unsafe {
        let w = &*wfx;
        if w.wFormatTag == WAVE_FORMAT_IEEE_FLOAT {
            return true;
        }
        if w.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
            let ext = &*(wfx as *const WAVEFORMATEXTENSIBLE);
            let sub_format = ext.SubFormat; // packed(1) 结构：按值拷出再比较
            return sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        }
        false
    }
}

pub(crate) fn list_microphone_targets() -> Result<Vec<CaptureTarget>> {
    // 调用线程侧 MTA 守卫贯穿整个枚举序列（参照 process.rs 的入口做法）
    let _com = caller_thread_com("麦克风枚举")?;
    let endpoints = capture_endpoints(&_com)?;
    Ok(endpoints
        .into_iter()
        .filter_map(|(device, name)| {
            if name.is_empty() {
                return None;
            }
            let _ = device; // id 用 FriendlyName（与 macOS cpal 路径一致），不用端点 ID
            Some(CaptureTarget {
                id: format!("mic:{name}"),
                name,
                kind: CaptureKind::Microphone,
                icon_path: None,
            })
        })
        .collect())
}

/// 整机 loopback：默认输出端点（eRender/eConsole）采集系统混音
pub(crate) fn spawn_device_loopback(
    tx: mpsc::Sender<AudioChunk>,
    counter: Arc<AtomicI64>,
) -> Result<StreamHandle> {
    // 调用线程侧 MTA 守卫贯穿：端点解析 → Activate → Initialize → Start
    // （守卫随本函数作用域存活，覆盖 spawn_endpoint_stream 内全部 COM 调用）
    let _com = caller_thread_com("整机 loopback")?;
    let enumerator = device_enumerator(&_com)?;
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| Error::AudioSource(format!("GetDefaultAudioEndpoint 失败: {e}")))?;
    spawn_endpoint_stream(
        "整机 loopback".to_string(),
        endpoint,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        tx,
        counter,
    )
}

/// 指定 FriendlyName 的麦克风
pub(crate) fn spawn_microphone(
    friendly_name: &str,
    tx: mpsc::Sender<AudioChunk>,
    counter: Arc<AtomicI64>,
) -> Result<StreamHandle> {
    let tag = format!("麦克风 {friendly_name}");
    // 调用线程侧 MTA 守卫贯穿：端点枚举匹配 → Activate → Initialize → Start
    let _com = caller_thread_com(&tag)?;
    let endpoints = capture_endpoints(&_com)?;
    let Some((device, _)) = endpoints
        .into_iter()
        .find(|(_, name)| name == friendly_name)
    else {
        return Err(Error::AudioSource(format!("未找到麦克风 {friendly_name}")));
    };
    spawn_endpoint_stream(
        tag,
        device,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        tx,
        counter,
    )
}

/// 端点 → Activate → GetMixFormat → 公共启动路径
fn spawn_endpoint_stream(
    tag: String,
    endpoint: IMMDevice,
    stream_flags: u32,
    tx: mpsc::Sender<AudioChunk>,
    counter: Arc<AtomicI64>,
) -> Result<StreamHandle> {
    let audio_client: IAudioClient = unsafe { endpoint.Activate(CLSCTX_ALL, None) }
        .map_err(|e| Error::AudioSource(format!("[{tag}] Activate(IAudioClient) 失败: {e}")))?;
    let mix_format = MixFormat(
        unsafe { audio_client.GetMixFormat() }
            .map_err(|e| Error::AudioSource(format!("[{tag}] GetMixFormat 失败: {e}")))?,
    );
    start_audio_client_stream(tag, audio_client, mix_format, stream_flags, HNS_100MS, tx, counter)
}

/// 已 Activate 的 IAudioClient 公共启动路径（Task 5 进程树 loopback 复用）：
/// float 校验 → Initialize → GetService(IAudioCaptureClient) → 事件句柄 → Start
/// → 子线程采集循环。
pub(crate) fn start_audio_client_stream(
    tag: String,
    audio_client: IAudioClient,
    mix_format: MixFormat,
    stream_flags: u32,
    buffer_hns: i64,
    tx: mpsc::Sender<AudioChunk>,
    counter: Arc<AtomicI64>,
) -> Result<StreamHandle> {
    let (channels, source_rate, format_tag) = unsafe {
        let w = &*mix_format.0;
        (w.nChannels, w.nSamplesPerSec, w.wFormatTag)
    };
    if channels == 0 || source_rate == 0 {
        return Err(Error::AudioSource(format!(
            "[{tag}] 无效 mix format（channels={channels}, rate={source_rate}）"
        )));
    }
    if !unsafe { mix_format_is_float(mix_format.0) } {
        return Err(Error::AudioSource(format!(
            "[{tag}] mix format 非 f32（tag={format_tag}），不支持"
        )));
    }

    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            stream_flags,
            buffer_hns,
            0,
            mix_format.0,
            None,
        )
    }
    .map_err(|e| Error::AudioSource(format!("[{tag}] Initialize 失败: {e}")))?;

    let capture: IAudioCaptureClient = unsafe { audio_client.GetService() }
        .map_err(|e| Error::AudioSource(format!("[{tag}] GetService(IAudioCaptureClient) 失败: {e}")))?;

    // WASAPI 要求 EVENTCALLBACK 配 auto-reset 事件
    let event = EventGuard(
        unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map_err(|e| Error::AudioSource(format!("[{tag}] CreateEventW 失败: {e}")))?,
    );
    unsafe { audio_client.SetEventHandle(event.0) }
        .map_err(|e| Error::AudioSource(format!("[{tag}] SetEventHandle 失败: {e}")))?;
    unsafe { audio_client.Start() }
        .map_err(|e| Error::AudioSource(format!("[{tag}] Start 失败: {e}")))?;

    tracing::info!("[{tag}] 采集启动: {channels}ch {source_rate}Hz");

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let audio_client = MtaInterface(audio_client);
    let capture = MtaInterface(capture);
    let tag_for_error = tag.clone();
    let join = std::thread::Builder::new()
        .name(format!("wasapi-{tag}"))
        .spawn(move || {
            capture_thread_main(
                audio_client,
                capture,
                event,
                tag,
                channels,
                source_rate,
                stop_flag,
                tx,
                counter,
            )
        })
        .map_err(|e| Error::AudioSource(format!("[{tag_for_error}] 采集线程创建失败: {e}")))?;

    Ok(StreamHandle {
        stop,
        join: Some(join),
    })
}

/// 采集线程主体：线程内 COM 初始化 → 采集循环 → Stop。
/// 接口经 MtaInterface 整体移入（精确捕获会绕过包装的 Send 实现，故收拢到函数边界）。
fn capture_thread_main(
    audio_client: MtaInterface<IAudioClient>,
    capture: MtaInterface<IAudioCaptureClient>,
    event: EventGuard,
    tag: String,
    channels: u16,
    source_rate: u32,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<AudioChunk>,
    counter: Arc<AtomicI64>,
) {
    let audio_client = audio_client.0;
    let capture = capture.0;
    // 采集线程自身初始化 COM（ComGuard 容忍 RPC_E_CHANGED_MODE）
    let Ok(_com) = ComGuard::new() else {
        tracing::error!("[{tag}] 采集线程 CoInitializeEx 失败");
        return;
    };
    run_event_capture_loop(
        &tag,
        &audio_client,
        &capture,
        &event,
        channels,
        source_rate,
        &stop,
        &tx,
        &counter,
    );
    if let Err(e) = unsafe { audio_client.Stop() } {
        tracing::warn!("[{tag}] Stop 失败: {e}");
    }
}

/// event-driven 采集循环（等待事件 → 取 f32 交错帧 → 转码 16k mono i16 → 下发）。
/// 每轮最多等 200ms（超时兜底轮询 stop）；错误已 tracing::error，返回后由调用方 Stop。
fn run_event_capture_loop(
    tag: &str,
    audio_client: &IAudioClient,
    capture: &IAudioCaptureClient,
    event: &EventGuard,
    channels: u16,
    source_rate: u32,
    stop: &AtomicBool,
    tx: &mpsc::Sender<AudioChunk>,
    counter: &AtomicI64,
) {
    let mut resampler: Option<rubato::Async<f32>> = None;
    loop {
        // 等待缓冲数据事件，200ms 超时兜底轮询 stop
        unsafe { WaitForSingleObject(event.0, 200) };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let padding = match unsafe { audio_client.GetCurrentPadding() } {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("[{tag}] GetCurrentPadding 失败: {e}");
                break;
            }
        };
        if padding == 0 {
            continue;
        }

        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames: u32 = 0;
        let mut flags: u32 = 0;
        if let Err(e) = unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }
        {
            tracing::error!("[{tag}] GetBuffer 失败: {e}");
            break;
        }

        // SILENT 帧按全零处理，帧数照常累计（保持时间线连续）
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        let samples: Vec<f32> = if silent || data.is_null() {
            vec![0.0; frames as usize * channels as usize]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    data as *const f32,
                    frames as usize * channels as usize,
                )
            }
            .to_vec()
        };

        let pcm = frames_to_pcm_i16(&samples, channels, source_rate, &mut resampler);
        let mut send_ok = true;
        if !pcm.is_empty() {
            // 16k 下 1 样本 = 1/16 ms：预扣本 chunk 时长，返回值为 chunk 起始时间
            let content_time_ms =
                counter.fetch_add(pcm.len() as i64 * 1000 / 16000, Ordering::SeqCst);
            let chunk = AudioChunk {
                pcm,
                content_time_ms,
                wall_time_ms: chrono::Utc::now().timestamp_millis(),
            };
            // channel 已关闭（下游停止消费）即退出
            send_ok = tx.blocking_send(chunk).is_ok();
        }
        if let Err(e) = unsafe { capture.ReleaseBuffer(frames) } {
            tracing::error!("[{tag}] ReleaseBuffer 失败: {e}");
            break;
        }
        if !send_ok {
            tracing::info!("[{tag}] 下发通道已关闭，退出采集");
            break;
        }
    }
}
