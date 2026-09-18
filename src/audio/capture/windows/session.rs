use crate::{Error, Result};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, RPC_E_CHANGED_MODE};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioSessionControl, IAudioSessionControl2, IAudioSessionEnumerator,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

/// 每线程一次的 COM 初始化守卫：获取时调 CoInitializeEx(MTA)，Drop 调 CoUninitialize。
/// 两个获取变体：
/// - [`ComGuard::new`]（宽容）：线程已按其他并发模型初始化（RPC_E_CHANGED_MODE）时
///   沿用现状，不接管也不配对反初始化——适用于线程内自用、不向其他线程传递 COM
///   接口的场景（如采集线程，新线程首次初始化必然 MTA 成功）。
/// - [`ComGuard::acquire_mta`]（严格）：遇 RPC_E_CHANGED_MODE 返回 Err——音频捕获的
///   调用线程侧序列（端点解析 → Activate → Initialize → Start）必须运行在 MTA，
///   否则裸接口移交 MTA 采集线程不安全（MtaInterface 的 SAFETY 前提）。
/// （Task 6 上移至 windows/mod.rs 共享）
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

/// pid → 可执行文件名（不含路径）；查询失败（如受保护进程）返回 None
fn process_exe_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let queried =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(handle);
        if !queried {
            return None;
        }
        let full_path = HSTRING::from_wide(&buf[..len as usize]).to_string();
        full_path.rsplit(['\\', '/']).next().map(str::to_string)
    }
}

/// 枚举默认 render endpoint 上的音频会话 → (pid, exe 名) 列表
pub(crate) fn enumerate_audio_sessions() -> Result<Vec<(u32, String)>> {
    // 1. CoInitializeEx(None, COINIT_MULTITHREADED) 守卫（本文件私有的 ComGuard）
    let _com = ComGuard::new()
        .map_err(|e| Error::AudioSource(format!("CoInitializeEx failed: {e}")))?;

    // 2. CoCreateInstance::<IMMDeviceEnumerator>(&MMDeviceEnumerator)
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| Error::AudioSource(format!("CoCreateInstance(MMDeviceEnumerator) failed: {e}")))?;

    // 3. GetDefaultAudioEndpoint(eRender, eConsole) → Activate::<IAudioSessionManager2>()
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| Error::AudioSource(format!("GetDefaultAudioEndpoint failed: {e}")))?;
    let manager: IAudioSessionManager2 = unsafe { endpoint.Activate(CLSCTX_ALL, None) }
        .map_err(|e| Error::AudioSource(format!("Activate(IAudioSessionManager2) failed: {e}")))?;

    // 4. GetSessionEnumerator() → GetCount() → 逐个 GetSession(i) 转 IAudioSessionControl2
    let session_enum: IAudioSessionEnumerator = unsafe { manager.GetSessionEnumerator() }
        .map_err(|e| Error::AudioSource(format!("GetSessionEnumerator failed: {e}")))?;
    let count = unsafe { session_enum.GetCount() }
        .map_err(|e| Error::AudioSource(format!("GetSessionCount failed: {e}")))?;

    let mut sessions = Vec::new();
    for i in 0..count {
        let control: IAudioSessionControl = match unsafe { session_enum.GetSession(i) } {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("GetSession({}) failed: {e}", i);
                continue;
            }
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };

        // 5. GetProcessId() → pid；pid == 0（系统会话）跳过
        let pid = unsafe { control2.GetProcessId() }.unwrap_or(0);
        if pid == 0 {
            continue;
        }

        // 6. OpenProcess + QueryFullProcessImageNameW → 完整路径取文件名
        match process_exe_name(pid) {
            Some(exe_name) => sessions.push((pid, exe_name)),
            None => tracing::warn!("无法获取进程 {} 的可执行名，跳过", pid),
        }
    }

    // 7. 收集 (pid, exe_name)；结构性错误已在上方 map_err 成 Error::AudioSource
    Ok(sessions)
}
