use super::ComGuard;
use crate::{Error, Result};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, eConsole, eRender, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEnumerator, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

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
    // 1. CoInitializeEx(None, COINIT_MULTITHREADED) 守卫（super::ComGuard 宽容变体）
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
    let mut inactive = 0u32;
    for i in 0..count {
        let control: IAudioSessionControl = match unsafe { session_enum.GetSession(i) } {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("GetSession({}) failed: {e}", i);
                continue;
            }
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            tracing::warn!("GetSession({}) 转 IAudioSessionControl2 失败，跳过", i);
            continue;
        };

        // 5. GetState()：只保留活动会话（Win11 大量 GUI 应用启动即建非活动会话，
        //    不过滤会淹没音源列表）；活动判定后续按真机表现校准
        let state = unsafe { control2.GetState() }.unwrap_or_default();
        if state != AudioSessionStateActive {
            inactive += 1;
            continue;
        }

        // 6. GetProcessId() → pid；pid == 0（系统会话）跳过
        let pid = unsafe { control2.GetProcessId() }.unwrap_or(0);
        if pid == 0 {
            continue;
        }

        // 7. OpenProcess + QueryFullProcessImageNameW → 完整路径取文件名
        match process_exe_name(pid) {
            Some(exe_name) => sessions.push((pid, exe_name)),
            None => tracing::warn!("无法获取进程 {} 的可执行名，跳过", pid),
        }
    }
    if inactive > 0 {
        tracing::debug!("已隐藏 {} 个无音频活动的会话", inactive);
    }

    // 8. 收集 (pid, exe_name)；结构性错误已在上方 map_err 成 Error::AudioSource
    Ok(sessions)
}
